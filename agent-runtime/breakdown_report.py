"""拆书报告：把参考书已拆的章节卡写成一份可读、可编辑的全书报告。

千章的书一次放不进模型：宿主按段准备章节卡摘要（每段约 40 章），这里先逐段归纳（可并行），
再把各段归纳与统计数据合并成全书结论。只依据章节卡里的事实，不读整章正文；
没拆的章节如实写进说明，结论都标章号。
"""
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
from typing import Annotated

from langchain_core.messages import HumanMessage, SystemMessage
from pydantic import ConfigDict, Field, with_config
from typing_extensions import TypedDict

from model_limits import RUNTIME_LIMITS
from structured_output import StructuredOutputError, invoke_structured

# 长度与条数上限放得宽：模型常比建议写得多一点，报告不因为啰嗦判失败；简洁靠描述引导。
Text = Annotated[str, Field(strict=True, min_length=1, max_length=1500)]
ChapterNo = Annotated[int, Field(strict=False, ge=1)]

# 所有字段都必填：deepseek 等模型在可选的嵌套字段上容易只写出键名就结束。


@with_config(ConfigDict(extra="forbid", strict=True))
class ReportRecipeStep(TypedDict):
    position: Annotated[str, Field(strict=True, min_length=1, max_length=80, description="章位置或阶段，如“第1章”“第4–10章”")]
    tasks: Annotated[str, Field(strict=True, min_length=1, max_length=800,
                                description="这一段要完成的写法任务，用“主角”“对手”等角色位置，不写参考书人名与具体情节")]


@with_config(ConfigDict(extra="forbid", strict=True))
class SegmentBeat(TypedDict):
    chapter: ChapterNo
    what: Annotated[Text, Field(description="这一章发生了什么，一句话")]
    technique: Annotated[Text, Field(description="用了什么写法、为什么有效")]


@with_config(ConfigDict(extra="forbid", strict=True))
class SegmentDigest(TypedDict):
    stage: Annotated[Text, Field(description="这一段在全书里的阶段名，如“退婚受辱·立下三年之约”")]
    goal: Annotated[Text, Field(description="这一段主角的目标与读者的期待")]
    keyBeats: Annotated[list[SegmentBeat], Field(min_length=1, max_length=12, description="最关键的 3–6 个节点")]
    pacing: Annotated[Text, Field(description="爽点与钩子的节奏规律，引用章号")]
    characterMoments: Annotated[list[Text], Field(max_length=10, description="主角靠哪些选择与反应立住，带章号，3–5 条")]
    threads: Annotated[list[Text], Field(max_length=10, description="这一段埋下或回收的伏笔，带章号，3–5 条")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ReportStage(TypedDict):
    range: Annotated[Text, Field(description="章节范围，如“第1–40章”")]
    title: Text
    goal: Annotated[Text, Field(description="这一阶段要完成什么")]
    payoff: Annotated[Text, Field(description="这一阶段兑现给读者的东西")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ReportTechnique(TypedDict):
    name: Annotated[Text, Field(description="写法名称，如“先压后扬”")]
    how: Annotated[Text, Field(description="这本书具体怎么用的，作者能照着学的要点")]
    chapters: Annotated[list[ChapterNo], Field(min_length=1, max_length=12)]


@with_config(ConfigDict(extra="forbid", strict=True))
class BreakdownReport(TypedDict):
    overview: Annotated[Text, Field(description="这本书靠什么抓住读者：卖点与读者期待，一两句")]
    stages: Annotated[list[ReportStage], Field(min_length=1, max_length=20)]
    opening: Annotated[Text, Field(description="开篇几章各完成了什么任务、怎么留住读者")]
    characterEstablishment: Annotated[Text, Field(description="主角怎么立住：关键选择、反应与反差，带章号")]
    pacing: Annotated[Text, Field(description="节奏规律：多久一个爽点、钩子怎么下、压抑最长几章，引用统计与章号")]
    threads: Annotated[Text, Field(description="伏笔与信息释放：埋多少、隔多久回收、设定怎么放")]
    techniques: Annotated[list[ReportTechnique], Field(min_length=1, max_length=12, description="最值得学的 3–8 条")]
    recipe: Annotated[list[ReportRecipeStep], Field(max_length=12, description="可复用配方：按章位置写写法任务，只用“主角”“对手”这类角色位置，不写人名与具体情节")]
    caveats: Annotated[list[Text], Field(max_length=8, description="覆盖范围与数据不足之处")]


SEGMENT_RULES = """你在为网文作者拆解一本参考书，分析它是怎么写的。下面是这本书一段章节的章节卡摘要：
每章有一句话概括、关键事件、钩子（位置、类型、强度）、爽点（类型、强度、是否写了旁观者反应）、埋下与回收的伏笔、情绪起落。
1. 只依据给定数据归纳，不编造摘要里没有的情节；引用章节一律用给定的 no。
2. 讲写法，不复述剧情：每个关键节点说清这一章做了什么、用了什么写法、为什么能抓住读者。
3. 作者关注的方向（focus）不为空时，优先归纳与它相关的写法。
4. 文字里需要加引号时用中文引号“”或「」，不要用英文双引号。
输出必须符合 SegmentDigest 结构。"""

REPORT_RULES = """你在为网文作者写一份参考书的拆书报告。给你的是：书的统计数据（爽点密度、章末钩子比例、最长无爽点区间、伏笔回收、文风）、
开篇几章的章节卡摘要，以及按段归纳的结果。
1. 每个结论都要能对到章号；只依据给定数据，不凭印象补充，数据不够的地方写进 caveats。
2. 报告是给作者学方法用的：讲清这本书怎么抓读者、节奏怎么安排、主角怎么立住、伏笔怎么埋和收，以及作者能直接拿去用的写法。
3. 覆盖范围（coverage）里有没拆的章节时，在 caveats 里写明报告只基于哪些章；caveats 只写覆盖范围与数据不足，不评论输入数据的格式。
4. recipe 是可复用的写法配方：按章位置写写法任务，只用“主角”“对手”“配角”这类角色位置，不写参考书的人名、地名和具体情节；
   只写已拆章节对应的位置，没拆的章节不要补写配方步骤。
5. 作者关注的方向（focus）不为空时，相关部分写得更细。
6. 文字里需要加引号时用中文引号“”或「」，不要用英文双引号。
输出必须符合 BreakdownReport 结构。"""

MAP_WORKERS = 4


def _add(total, usage):
    for key in ("input_tokens", "output_tokens", "total_tokens"):
        total[key] = total.get(key, 0) + (usage or {}).get(key, 0)


def _invoke(model, schema, rules, payload):
    """模型偶尔返回格式不完整的结果（修复也救不回来时）：同一请求重试一次，用量照算。"""
    messages = [SystemMessage(content=rules), HumanMessage(content=json.dumps(payload, ensure_ascii=False))]
    spent = {}
    for attempt in range(RUNTIME_LIMITS.extraction_attempts):
        try:
            result = invoke_structured(model, schema, messages)
        except StructuredOutputError as error:
            _add(spent, error.usage_metadata)
            if error.code != "invalid_output" or attempt + 1 == RUNTIME_LIMITS.extraction_attempts:
                error.usage_metadata = spent
                raise
            continue
        _add(spent, getattr(result.get("raw"), "usage_metadata", None) or {})
        return result["parsed"], spent


def build(model, data, focus="", progress=None):
    """逐段归纳（并行）再合并。返回 (报告, 用量)。progress(完成数, 总数) 用于界面进度。"""
    segments = data.get("segments") or []
    total_steps = len(segments) + 1
    usage = {}
    digests = [None] * len(segments)

    def summarize(index):
        segment = segments[index]
        return index, _invoke(model, SegmentDigest, SEGMENT_RULES, {
            "book": data.get("title"), "range": f"第{segment['from']}–{segment['to']}章", "focus": focus,
            "chapters": segment["chapters"]})

    done = 0
    with ThreadPoolExecutor(max_workers=min(MAP_WORKERS, max(1, len(segments)))) as pool:
        for future in as_completed([pool.submit(summarize, i) for i in range(len(segments))]):
            index, (digest, used) = future.result()
            digests[index] = digest
            _add(usage, used)
            done += 1
            if progress:
                progress(done, total_steps)
    opening = (segments[0]["chapters"][:3] if segments else [])
    report, used = _invoke(model, BreakdownReport, REPORT_RULES, {
        "book": data.get("title"), "focus": focus,
        "coverage": {"range": f"第{data.get('from')}–{data.get('to')}章", "totalChapters": data.get("totalChapters"),
                     "indexedChapters": data.get("indexedChapters"), "notIndexed": data.get("missing") or []},
        "stats": data.get("stats"), "threads": data.get("threads"), "style": data.get("style"), "opening": opening,
        "segments": [{"range": f"第{s['from']}–{s['to']}章", **d} for s, d in zip(segments, digests)]})
    _add(usage, used)
    if progress:
        progress(total_steps, total_steps)
    return report, usage


def _chapters(start, end):
    return f"第 {start} 章" if start == end else f"第 {start}–{end} 章"


def _pct(value):
    return f"{round((value or 0) * 100)}%"


def render(data, report):
    """报告的 Markdown 正文：作者在成果面板里看到、能直接改的就是这份。"""
    stats = data.get("stats") or {}
    gap = stats.get("longestPayoffGap") or {}
    lines = [f"# 《{data.get('title')}》拆书报告", "",
             f"> 基于第 {data.get('from')}–{data.get('to')} 章的章节卡（已拆 {data.get('indexedChapters')} 章，全书 {data.get('totalChapters')} 章）。"
             "人物和情节只是分析对象，借的是写法。", "",
             "## 一句话看懂", "", report["overview"], "",
             "## 阶段划分", "", "| 章节 | 阶段 | 要完成什么 | 兑现给读者 |", "| --- | --- | --- | --- |"]
    cell = lambda text: str(text).replace("|", "／").replace("\n", " ")
    lines += [f"| {cell(s['range'])} | {cell(s['title'])} | {cell(s['goal'])} | {cell(s['payoff'])} |" for s in report["stages"]]
    lines += ["", "## 开篇怎么写", "", report["opening"], "",
              "## 主角怎么立住", "", report["characterEstablishment"], "",
              "## 节奏与爽点", "",
              f"每章约 {stats.get('payoffsPerChapter', 0)} 个爽点，章末留钩子的章节占 {_pct(stats.get('endHookRate'))}"
              + (f"，最长连续 {gap.get('length')} 章没有爽点（{_chapters(gap.get('from'), gap.get('to'))}）。" if gap.get("length") else "。"),
              "", report["pacing"], "",
              "## 伏笔与信息释放", "", report["threads"], "",
              "## 可以学的写法", ""]
    lines += [f"- **{t['name']}**：{t['how']}（第 {'、'.join(str(c) for c in t['chapters'])} 章）" for t in report["techniques"]]
    if report["recipe"]:
        lines += ["", "## 写法配方", ""]
        lines += [f"{i}. {step['position']}：{step['tasks']}" for i, step in enumerate(report["recipe"], start=1)]
    if report["caveats"]:
        lines += ["", "## 说明", ""]
        lines += [f"- {item}" for item in report["caveats"]]
    return "\n".join(lines) + "\n"
