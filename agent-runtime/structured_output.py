"""领域类型到 SDK 的唯一结构化输出边界。

SDK 接收 Pydantic 生成的 JSON Schema，避免其 TypedDict 转换丢失 Required/Annotated
语义。返回值仍由原类型校验；传 JSON Schema 不意味着放弃本地类型和字段约束。
"""
import json
import re
from functools import lru_cache

from jsonschema import Draft202012Validator
from pydantic import BaseModel, TypeAdapter, ValidationError
from request_budget import invoke_model


@lru_cache(maxsize=32)
def _adapter(schema):
    return TypeAdapter(schema)


def structured_model(model, schema):
    return model.with_structured_output(
        _adapter(schema).json_schema(), method="function_calling", include_raw=True
    )


@lru_cache(maxsize=32)
def _validator(schema):
    return Draft202012Validator(_adapter(schema).json_schema())


class StructuredOutputError(ValueError):
    """Public errors contain safe metadata; an optional local snapshot supports replay."""
    def __init__(self, schema, result, detail="无法解析", code="invalid_output", issues=()):
        raw = result.get("raw")
        metadata = getattr(raw, "response_metadata", None) or {}
        reason = metadata.get("finish_reason")
        if detail == "无法解析" and getattr(raw, "invalid_tool_calls", None):
            detail = "格式不完整（模型返回的 JSON 无法解析）"
        if reason == "length":
            detail, code = "达到输出长度上限，未得到完整结果", "output_limit"
        elif reason in ("content_filter",):
            detail, code = "被模型服务过滤，未得到完整结果", "filtered_output"
        self.code = code
        self.schema_name = schema.__name__
        self.output_mode = result.get("output_mode")
        parsed = result.get("parsed")
        if isinstance(parsed, BaseModel):
            parsed = parsed.model_dump(mode="json")
        self.response_snapshot = {"finishReason": reason, "parsed": parsed} if parsed is not None else {
            "finishReason": reason, "content": getattr(raw, "content", None),
            "invalidToolCalls": getattr(raw, "invalid_tool_calls", None)}
        self.issues = list(issues)
        self.model_calls = result.get("model_calls", 1)
        self.usage_metadata = {k: v for k, v in (getattr(raw, "usage_metadata", None) or {}).items()
                               if k in ("input_tokens", "output_tokens", "total_tokens") and isinstance(v, int)}
        self.finish_reason = reason if reason in ("length", "stop", "tool_calls", "content_filter") else None
        super().__init__(f"{schema.__name__} 结构化结果{detail}" + (f"（结束原因：{self.finish_reason}）" if self.finish_reason else ""))


def validation_issue(error):
    """Keep paths, constraints and sizes, never the offending prose/provider body."""
    # Nullable objects wrap their real field error in anyOf. Report the
    # non-null branch's concrete limit instead of an unactionable union error.
    if error.validator == "anyOf" and error.instance is not None:
        branches = error.validator_value
        non_null = [index for index, branch in enumerate(branches) if branch.get("type") != "null"]
        if len(branches) == 2 and len(non_null) == 1:
            detail = next((child for child in error.context
                           if child.schema_path and child.schema_path[0] == non_null[0]), None)
            if detail is not None:
                return validation_issue(detail)
    value, rule = error.instance, error.validator
    path = ".".join(map(str, error.absolute_path)) or "$"
    issue = {"path": path, "rule": rule}
    if rule in ("maxItems", "minItems", "maxLength", "minLength"):
        issue.update(expected=error.validator_value, actual=len(value))
        unit = "项" if rule.endswith("Items") else "字符"
        bound = "最多" if rule.startswith("max") else "至少"
        message = f"{path} {bound} {issue['expected']} {unit}，实际 {issue['actual']} {unit}"
    elif rule == "required":
        missing = [key for key in error.validator_value if key not in value]
        issue["expected"] = missing
        message = f"{path} 缺少必填字段：{', '.join(missing)}"
    elif rule == "type":
        issue.update(expected=error.validator_value, actual=type(value).__name__)
        message = f"{path} 需要 {issue['expected']}，实际类型 {issue['actual']}"
    else:
        # enum values/patterns may themselves contain user text; only return
        # the rule name. The full schema is already supplied to the model.
        message = f"{path} 不符合 {rule} 约束，请按 schema 修正"
    issue["message"] = message
    return issue


# 末尾只剩键名和冒号；deepseek 在嵌套的可选字段上会写出两个冒号（`"storyTime"::`）后结束
_DANGLING_KEY = re.compile(r',\s*"[A-Za-z_][A-Za-z0-9_]*"\s*:(?:\s*:)?\s*$')


def repair_dangling_key(args):
    """模型正常结束却在最后一个字段只写出键名（如 `..., "storyTime":`、`..., "storyTime"::`）时，去掉这个空键、补齐括号再解析。

    只修这一种截断：键后面一个字都没有，丢掉的只是一个尚未开始的字段；值写了一半、字符串没闭合的一律不修。
    修好的结果照常经过契约校验，缺必填字段仍会失败。返回 None 表示修不了。
    """
    if not isinstance(args, str):
        return None
    text = args.rstrip()
    match = _DANGLING_KEY.search(text)
    if not match:
        return None
    text = text[:match.start()]
    closers, in_string, escaped = [], False, False
    for ch in text:
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
        elif ch == '"':
            in_string = True
        elif ch in "{[":
            closers.append("}" if ch == "{" else "]")
        elif ch in "}]":
            if not closers or closers.pop() != ch:
                return None
    if in_string:
        return None
    try:
        value = json.loads(text + "".join(reversed(closers)))
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def escape_stray_quotes(text):
    """字符串里没转义的英文双引号（中文里常拿来加强调：`让"穿越者"这类设定`）转义掉。

    合法 JSON 里字符串的结束引号后面只会跟 `,` `}` `]` `:`（中间可有空白）或文本结尾；
    后面跟别的字符的引号只能是正文里的引号。对合法 JSON 不做任何改动。
    """
    out, in_string, escaped, i = [], False, False, 0
    while i < len(text):
        ch = text[i]
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                j = i + 1
                while j < len(text) and text[j] in " \t\r\n":
                    j += 1
                if j < len(text) and text[j] not in ",}]:":
                    out.append('\\"')
                    i += 1
                    continue
                in_string = False
        elif ch == '"':
            in_string = True
        out.append(ch)
        i += 1
    return "".join(out)


def first_object(text):
    """取文本里第一个完整的 JSON 对象，后面多出的括号、围栏或说明文字不影响它（模型常在结尾多写一个 `}`）。

    只接受完整、合法的对象：写到一半的照样失败，结果照常经过契约校验。"""
    if not isinstance(text, str):
        return None
    start = text.find("{")
    if start < 0:
        return None
    try:
        value, _ = json.JSONDecoder().raw_decode(text, start)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def drop_empty_keys(value):
    """键名为空串的字段（如 `"": ""`）不可能是契约里的字段，去掉后再校验，不让一个空键废掉整份结果。"""
    if isinstance(value, dict):
        return {key: drop_empty_keys(item) for key, item in value.items() if key != ""}
    if isinstance(value, list):
        return [drop_empty_keys(item) for item in value]
    return value


def repair_tool_arguments(args):
    """模型正常结束但函数参数不是合法 JSON 时的有限修复：先转义正文里的英文双引号，再去掉末尾只有键名的空字段。"""
    if not isinstance(args, str):
        return None
    text = escape_stray_quotes(args)
    try:
        value = json.loads(text)
        return value if isinstance(value, dict) else None
    except json.JSONDecodeError:
        return first_object(text) or repair_dangling_key(text)


def _repaired(schema, result):
    """只在模型正常结束（不是被长度截断）且只调用了这一个结构化函数时尝试修复。"""
    raw = result.get("raw")
    reason = (getattr(raw, "response_metadata", None) or {}).get("finish_reason")
    if reason not in ("tool_calls", "stop") or getattr(raw, "tool_calls", None):
        return None
    title = _adapter(schema).json_schema().get("title")
    calls = [t for t in getattr(raw, "invalid_tool_calls", None) or [] if t.get("name") == title]
    return repair_tool_arguments(calls[0].get("args")) if len(calls) == 1 else None


# 供应商自己的函数调用标记漏进参数里（DeepSeek V4 的 DSML 写法），参数根本不是 JSON：
# 这不是"格式不完整"，是模型这一次没有作答。原样重发即可，不必带纠错提示。
# 只认这些标记和空调用；写到一半的 JSON 仍走原来的修复与报错，不白白重发。
_DEGENERATE_MARKERS = ("DSML", "<parameter", "_placeholder", "<｜", "｜>")
TRANSIENT_ATTEMPTS = 3
_USAGE_KEYS = ("input_tokens", "output_tokens", "total_tokens")


def degenerate_tool_call(result):
    """函数调用路上模型没给出结果：空调用，或参数极短 / 带供应商标记且不是 JSON。"""
    if result.get("parsed") is not None:
        return False
    raw = result.get("raw")
    if getattr(raw, "tool_calls", None):
        return False
    calls = getattr(raw, "invalid_tool_calls", None) or []
    if not calls:
        reason = (getattr(raw, "response_metadata", None) or {}).get("finish_reason")
        return reason == "tool_calls" and not (getattr(raw, "content", None) or "")
    args = calls[0].get("args")
    if not isinstance(args, str) or not args.strip():
        return True
    try:
        json.loads(args)
        return False
    except json.JSONDecodeError:
        return any(marker in args for marker in _DEGENERATE_MARKERS)


def _usage_of(raw):
    return {k: v for k, v in (getattr(raw, "usage_metadata", None) or {}).items() if k in _USAGE_KEYS and isinstance(v, int)}


def _merge_usage(total, raw):
    for key, value in _usage_of(raw).items():
        total[key] = total.get(key, 0) + value
    return total


def extract_json_object(text):
    """正文里的 JSON：去掉代码围栏和前后的话，取最外层花括号之间的部分；修不了返回 None。"""
    if not isinstance(text, str):
        return None
    body = re.sub(r"^\s*```(?:json)?\s*|\s*```\s*$", "", text.strip())
    start, end = body.find("{"), body.rfind("}")
    if start < 0 or end <= start:
        return None
    candidate = body[start:end + 1]
    try:
        value = json.loads(candidate)
    except json.JSONDecodeError:
        value = first_object(body) or repair_tool_arguments(candidate)
    return value if isinstance(value, dict) else None


def _json_document(model, schema, messages, *, native=False, request_options=None):
    """Document transport: JSON mode is a request parameter, not a tool call.

    Budget the actual rendered messages (including schema). The plain-text path
    remains available for providers without a declared native JSON capability.
    """
    from langchain_core.messages import SystemMessage
    from model_limits import prepare_request
    instruction = "只输出一个 json 对象，不要其他文字或代码围栏。字段必须直接位于该对象中，不要增加包装字段。遵守此 JSON Schema：" + json.dumps(_adapter(schema).json_schema(), ensure_ascii=False)
    rendered = list(messages)
    if rendered and isinstance(rendered[0], SystemMessage) and isinstance(rendered[0].content, str):
        rendered[0] = SystemMessage(content=rendered[0].content + "\n\n" + instruction)
    else:
        rendered.insert(0, SystemMessage(content=instruction))
    model = prepare_request(model, rendered)
    options = dict(request_options or {})
    if native:
        options["response_format"] = {"type": "json_object"}
    raw = invoke_model(model, rendered, **options)
    content = raw.content if isinstance(raw.content, str) else "".join(
        block.get("text", "") for block in raw.content if isinstance(block, dict))
    parsed = None
    if native:
        try:
            value = json.loads(content)
            parsed = value if isinstance(value, dict) else None
        except (ValueError, TypeError):
            parsed = first_object(content)
    else:
        parsed = extract_json_object(content)
    return {"raw": raw, "parsed": parsed, "parsing_error": None}


def project_document_fields(value, schema, root=None):
    """Project derived model data onto its declared fields, without coercion.

    Only callers producing non-authoritative analysis opt in. Missing fields,
    wrong types, invalid enums and evidence references remain validation errors.
    """
    root = schema if root is None else root
    if '$ref' in schema:
        target=root
        for part in schema['$ref'].removeprefix('#/').split('/'):
            target=target[part]
        return project_document_fields(value,target,root)
    if isinstance(value,dict) and schema.get('type')=='object':
        properties=schema.get('properties',{})
        return {key:project_document_fields(item,properties.get(key,{}),root)
                for key,item in value.items() if key in properties or schema.get('additionalProperties') is not False}
    if isinstance(value,list) and schema.get('type')=='array':
        return [project_document_fields(item,schema.get('items',{}),root) for item in value]
    return value


def invoke_structured(model, schema, messages, drop_fields=(), *, max_calls=None, mode=None, project_fields=False):
    """`drop_fields`：这次调用用不上的顶层块（如初评时的修订评估）。模型若把它填残，整份结果不该作废：
    校验前直接去掉这些块，其余字段仍按契约逐项校验。
    模型"没作答"（见 degenerate_tool_call）时原样重发，最多 TRANSIENT_ATTEMPTS 次，仍不行就改要正文 JSON；
    几次的用量合并记在最终返回的 raw 上。"""
    from model_limits import prepare_request
    if max_calls is not None and max_calls < 1:
        raise ValueError("max_calls 必须大于零")
    # Per-request headers must reach the SDK client; model_copy(default_headers)
    # does not rebuild its already-created HTTP client.
    request_options = {"extra_headers": {"X-EW-Max-Attempts": "1"}} if max_calls is not None else {}
    mode = mode or (getattr(model, "metadata", None) or {}).get("ew_structured_output", "tool")
    if mode not in ("tool", "json", "json_object"):
        raise ValueError("不支持的结构化输出通道")
    if mode == "tool":
        model = prepare_request(model, messages, _adapter(schema).json_schema())
    usage = {}
    result = None
    calls = 0
    if mode in ("json", "json_object"):
        result = _json_document(model, schema, messages, native=mode == "json_object", request_options=request_options)
        calls = 1
        _merge_usage(usage, result.get("raw"))
    else:
        for _ in range(min(TRANSIENT_ATTEMPTS, max_calls or TRANSIENT_ATTEMPTS)):
            result = invoke_model(structured_model(model, schema), messages, **request_options)
            calls += 1
            _merge_usage(usage, result.get("raw"))
            if not degenerate_tool_call(result):
                break
        else:
            if max_calls is None or calls < max_calls:
                result = _json_document(model, schema, messages, request_options=request_options)
                calls += 1
                _merge_usage(usage, result.get("raw"))
            if result.get("parsed") is None:
                raise StructuredOutputError(schema, {**result, "model_calls": calls, "raw": _with_usage(result.get("raw"), usage)},
                                            "没有给出结果（函数调用参数为空或包含供应商标记）", code="transient_output")
    result = {**result, "model_calls": calls, "output_mode": mode}
    if mode in ("json", "json_object") and result.get("parsed") is None:
        empty = not (getattr(result.get("raw"), "content", None) or "")
        raise StructuredOutputError(schema, {**result, "raw": _with_usage(result.get("raw"), usage)},
                                    "没有返回正文" if empty else "没有给出有效 JSON",
                                    code="empty_output" if empty else "invalid_output")
    if usage and getattr(result.get("raw"), "usage_metadata", None) != usage:
        result = {**result, "raw": _with_usage(result.get("raw"), usage)}
    if result.get("parsing_error") is not None or result.get("parsed") is None:
        repaired = _repaired(schema, result)
        if repaired is None:
            raise StructuredOutputError(schema, result)
        result = {**result, "parsed": repaired, "parsing_error": None}
    value = result["parsed"]
    if isinstance(value, BaseModel):
        value = value.model_dump(mode="json")
    value = drop_empty_keys(value)
    if drop_fields and isinstance(value, dict):
        value = {key: item for key, item in value.items() if key not in drop_fields}
    if project_fields:
        value = project_document_fields(value, _adapter(schema).json_schema())
    errors = list(_validator(schema).iter_errors(value))[:5]
    if errors:
        issues = [validation_issue(error) for error in errors]
        raise StructuredOutputError(schema, result, "不符合契约：" + "；".join(i["message"] for i in issues), issues=issues)
    try:
        parsed = _adapter(schema).validate_python(value)
    except ValidationError as error:
        fields = [".".join(map(str, issue["loc"])) or "$" for issue in error.errors(include_input=False)[:5]]
        # 不把模型响应正文或 SDK 原始错误回显到事件日志。
        raise StructuredOutputError(schema, result, f"不符合契约：{', '.join(fields)}") from None
    return {**result, "parsed": parsed}


def _with_usage(raw, usage):
    """把几次尝试的用量合并写回消息，调用方照旧从 raw.usage_metadata 记账。"""
    if raw is None:
        return raw
    try:
        raw.usage_metadata = {**(getattr(raw, "usage_metadata", None) or {}), **usage}
    except (AttributeError, TypeError, ValueError):
        pass
    return raw
