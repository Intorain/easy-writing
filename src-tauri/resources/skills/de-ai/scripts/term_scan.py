"""扫描正文里的模板化用词与句式，给定向重写定位。只用标准库。

输入（标准输入 JSON）：
  text   正文，段落以换行分隔
  terms  可选，额外要查的词（作者的禁用词）
输出（标准输出 JSON）：
  words        正文字数（不含空白）
  per1k        每千字命中次数
  hits         每个词的次数、所在段落（从 0 开始）与至多两个例句
  parallel     三句同起的排比所在段落
  summaryTails 以抽象总结收尾的段落
"""
import json
import re
import sys

DEFAULT_TERMS = [
    "仿佛", "宛如", "不禁", "一丝", "一抹", "眼中闪过", "嘴角勾起", "空气仿佛凝固", "心中一凛", "缓缓", "轻轻",
    "深吸一口气", "不由得", "莫名", "深邃", "复杂的情绪", "不由自主", "下意识", "眸光", "嘴角微微上扬", "难以言喻",
]
SUMMARY_MARKERS = ["这不仅是", "更是", "从此", "注定", "命运", "一切都", "终于明白"]
SENTENCE_END = re.compile(r"(?<=[。！？!?…])")


def sentences(paragraph):
    return [s.strip() for s in SENTENCE_END.split(paragraph) if s.strip()]


def main():
    data = json.load(sys.stdin)
    text = str(data.get("text") or "")
    terms = list(dict.fromkeys(DEFAULT_TERMS + [str(t) for t in data.get("terms") or [] if str(t).strip()]))
    paragraphs = [p.strip() for p in text.split("\n") if p.strip()]
    words = sum(1 for c in text if not c.isspace())
    hits = []
    for term in terms:
        where = [i for i, p in enumerate(paragraphs) if term in p]
        if not where:
            continue
        examples = [s for i in where for s in sentences(paragraphs[i]) if term in s][:2]
        hits.append({"term": term, "count": text.count(term), "paragraphs": where[:30], "examples": examples})
    hits.sort(key=lambda h: -h["count"])
    parallel = []
    for i, p in enumerate(paragraphs):
        heads = [s[:2] for s in sentences(p)]
        if any(len(heads[j]) == 2 and heads[j] == heads[j + 1] == heads[j + 2] for j in range(len(heads) - 2)):
            parallel.append(i)
    tails = [i for i, p in enumerate(paragraphs)
             if sentences(p) and not sentences(p)[-1].startswith("“") and any(m in sentences(p)[-1] for m in SUMMARY_MARKERS)]
    total = sum(h["count"] for h in hits)
    print(json.dumps({"words": words, "per1k": round(total * 1000 / words, 1) if words else 0, "hits": hits,
                      "parallel": parallel, "summaryTails": tails}, ensure_ascii=False))


if __name__ == "__main__":
    main()
