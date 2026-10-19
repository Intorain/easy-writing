"""工具分组披露：每次模型调用只带常用工具加已打开的领域组，不把全部工具定义塞进每一次请求。

分组是静态表，打开状态存在会话状态里（随检查点跨轮保留）；模型用 open_toolset 自己打开，
不按作者措辞猜。宿主与工具本身不变：工具节点仍登记全部工具，只是发给模型的清单变短。
"""
from typing import Annotated, Callable, Literal, NotRequired

from langchain.agents.middleware import AgentMiddleware, AgentState, ModelRequest
from langchain_core.messages import ToolMessage
from langchain_core.tools import InjectedToolCallId, tool
from langgraph.types import Command

# 任何对话都可能用到的工具：读作品、问作者、看成果、带书进对话、打开工具组
CORE_TOOLS = {
    "ask_author", "open_toolset", "list_books", "open_book", "load_book_context", "select_write_target", "begin_new_task",
    "list_volumes", "list_chapters", "read_chapter", "read_book_materials", "search_book", "recall_memory",
    "list_artifacts", "read_artifact", "save_artifact", "list_skills", "read_skill", "read_skill_resource", "run_skill_script",
}

# 领域组：编号、一句话（给模型看）、工具名
TOOLSETS: dict[str, tuple[str, set[str]]] = {
    "rank": ("榜单与选题：查榜单目录、取榜单、跨分类汇总、本地快照与趋势、选题卡、发趋势图", {
        "list_rank_sources", "get_rank", "get_rank_collection", "rank_trends", "save_selection_cards", "send_rank_chart",
    }),
    "plan": ("开书与资料：开书草案与采用、人物 / 大纲 / 设定资料、本书写作要求、写作画像、取名、简介、封面", {
        "prepare_book_plan", "adopt_book_plan", "save_material", "apply_creation_artifact",
        "configure_book_writing", "list_writing_profiles", "save_name_candidates", "save_book_intro", "request_cover", "generate_cover",
    }),
    "write": ("写作与改稿：规则集、写作上下文、拍子与章纲、起草、修订、体检、保存正文、短篇、文风、发章节文件", {
        "resolve_ruleset", "assemble_context", "plan_scenes", "plan_chapters", "draft_chapter", "polish_draft", "revise_spans",
        "check_chapter", "open_chapter_candidate", "save_chapter", "plan_short_story", "draft_short_story",
        "read_style_profile", "propose_writing_preference", "send_chapter_file",
    }),
    "catalog": ("卷章与作品信息：新建 / 改名 / 删除卷与章（进回收站），改书名简介分类标签，删作品", {
        "create_volume", "create_chapter", "update_volume", "update_chapter", "update_book",
        "delete_volume", "delete_chapter", "delete_book",
    }),
    "story": ("故事状态与规矩：线索账本、人物状态、时间线、故事线、前文修改影响、长期规则、疑点裁决、创作决定记忆", {
        "list_threads", "update_thread", "read_character_state", "read_timeline", "read_storylines", "impact_report",
        "save_rule", "record_suspect_verdict", "record_book_memory", "get_memory_sources",
    }),
    "breakdown": ("拆书与蒸馏：参考书目录与画像、写法案例、拆书报告、全书或指定范围蒸馏、进度与原文证据回查、写法配方、仿书对照", {
        "prepare_distillation", "read_distillation", "list_breakdowns", "read_breakdown_profile", "search_breakdown_cases", "compare_pacing", "start_breakdown",
        "breakdown_status", "read_material", "write_breakdown_report", "save_breakdown_recipe", "save_comparison", "read_pacing",
    }),
    "skills": ("技能管理：整理技能候选、卸载技能", {"prepare_skill", "uninstall_skill"}),
    "notes": ("灵感、统计、工作备注与自动任务：灵感便签、码字统计、工作备注、定时任务的查看 / 建立 / 删除", {
        "list_inspirations", "add_inspiration", "writing_stats", "work_note", "list_automations", "create_automation", "delete_automation",
    }),
}

ToolsetName = Literal["rank", "plan", "write", "catalog", "story", "breakdown", "skills", "notes"]
DOMAIN_OF = {name: key for key, (_, names) in TOOLSETS.items() for name in names}


def summary_line(key: str) -> str:
    return f"{key}：{TOOLSETS[key][0]}"


def prompt_section() -> str:
    """系统提示里的分组说明：组名与一句话，模型据此决定先打开哪一组。"""
    lines = ["# 工具分组",
             "工具按领域分组，每次只带常用的和已打开的组；要用下面某组的工具时先用 open_toolset 打开（一次可以开几组），打开后本会话一直可用。",
             "工具清单里没有的能力，先看是不是在某个没打开的组里；都不在就说明还没接入，不编造执行结果。"]
    lines.extend(f"- {summary_line(key)}" for key in TOOLSETS)
    return "\n".join(lines)


def merge_toolsets(current: list[str] | None, update: list[str] | None) -> list[str]:
    """状态归并：打开只增不减，顺序按首次打开。"""
    merged = list(current or [])
    for key in update or []:
        if key not in merged:
            merged.append(key)
    return merged


class ToolsetState(AgentState):
    """已打开的领域组；随检查点保留，跨轮不用重开。"""
    toolsets: NotRequired[Annotated[list[str], merge_toolsets]]


class ToolsetMiddleware(AgentMiddleware):
    """发给模型的工具清单 = 常用工具 + 框架自带工具 + 已打开领域组的工具；工具节点不受影响。"""

    state_schema = ToolsetState

    def wrap_model_call(self, request: ModelRequest, handler):
        return handler(request.override(tools=visible_tools(request.tools, request.state.get("toolsets"))))


def visible_tools(tools, opened: list[str] | None):
    active = set(opened or [])
    return [t for t in tools if DOMAIN_OF.get(getattr(t, "name", "")) in (None, *active)]


def first_sentence(text: str) -> str:
    head = (text or "").strip().split("\n", 1)[0]
    for mark in ("。", "；", "."):
        if mark in head:
            return head.split(mark, 1)[0] + ("。" if mark != "." else ".")
    return head


def make_open_toolset(available: Callable[[], dict[str, object]]):
    """生成 open_toolset：`available` 在调用时给出本次对话实际可用的工具表（按入口能力过滤过的），
    打开后把该组工具的名字与一句话列给模型，不用再猜有什么。"""

    def describe(domains) -> str:
        table = available()
        parts = []
        for key in domains:
            if key not in TOOLSETS:
                parts.append(f"没有叫 {key} 的工具组。可用：{'、'.join(TOOLSETS)}")
                continue
            lines = [f"已打开 {summary_line(key)}"]
            for name in sorted(n for n in TOOLSETS[key][1] if n in table):
                lines.append(f"- {name}：{first_sentence(getattr(table[name], 'description', ''))}")
            parts.append("\n".join(lines))
        return "\n\n".join(parts) or "没有打开任何工具组。"

    @tool
    def open_toolset(domains: Annotated[list[ToolsetName], "要打开的工具组，可多个"],
                     tool_call_id: Annotated[str, InjectedToolCallId]) -> Command:
        """打开一组或几组领域工具（榜单与选题 rank、开书与资料 plan、写作与改稿 write、卷章与作品信息 catalog、
        故事状态与规矩 story、拆书与仿书 breakdown、技能管理 skills、灵感与统计 notes）。
        打开后这些工具出现在你的工具清单里，本会话一直可用；返回里列出每个工具的一句话说明。"""
        return Command(update={"toolsets": [d for d in domains if d in TOOLSETS],
                               "messages": [ToolMessage(content=describe(domains), tool_call_id=tool_call_id, name="open_toolset")]})

    return open_toolset
