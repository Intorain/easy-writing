"""Plain-text helpers shared by writing tools: no model, no judgement."""


def tail_by_paragraph(text, max_chars):
    """从末尾按段落取不超过 max_chars 的文字，不在句中切断；与宿主 agent_context::tail_by_paragraph 同口径。"""
    paragraphs = [p.strip() for p in text.split("\n") if p.strip()]
    kept, count = [], 0
    for paragraph in reversed(paragraphs):
        length = len(paragraph)
        if count + length > max_chars and kept:
            break
        if count + length > max_chars:
            kept.append(paragraph[-max_chars:])
            break
        kept.append(paragraph)
        count += length
    kept.reverse()
    return "\n".join(kept)


def word_count(text):
    """与宿主 agent_quality::metrics、agent_service::words_of 同口径：去掉空白与 BOM 后的字符数。"""
    return sum(1 for c in text if not c.isspace() and c != "﻿")
