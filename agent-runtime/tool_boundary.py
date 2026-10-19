"""Reject invalid arguments before effects; return repairable failures to the existing agent loop."""
import json
from pathlib import Path
from jsonschema import Draft202012Validator
from langchain.agents.middleware import wrap_tool_call
from langchain_core.messages import ToolMessage
from host import ToolFailure
from tool_results import normalize_tool_message, error_payload
from model_protocol import rejected_call, invalid_call_result
from writer_output import RoleOutputError


RETRY_LIMIT = 3


def retry_limit(call_id, name, message):
    """同一工具连续失败超过上限：这次不执行，回执告诉模型停手。task_progress 读到它就收起工具，
    让模型向作者说明后正常结束，不抛异常把整轮判失败。"""
    return ToolMessage(tool_call_id=call_id, name=name, status="error", content=json.dumps({"ok": False, "error": {
        "code": "tool_retry_limit", "retryable": False, "outcome": "not_applied",
        "message": message + "不要再调用这个工具，直接向作者说明已完成的部分、卡在哪里、需要作者做什么。"}}, ensure_ascii=False))


def schemas():
    return json.loads(Path(__file__).with_name("tool-contracts.json").read_text(encoding="utf-8"))


def argument_issues(validator, args):
    errors = sorted(validator.iter_errors(args), key=lambda error: str(list(error.path)))
    # Conditional branches pinpoint actual fields instead of repeating the union's whole-object error.
    precise = [e for e in errors if e.validator != "anyOf"]
    errors = precise or errors
    return [{"field": ".".join(map(str, error.path)) or "$", "message": error.message[:240]}
            for error in errors[:8]]


def contract_middleware():
    validators = {name: Draft202012Validator(schema) for name, schema in schemas()["model"].items()}
    failures = {}

    @wrap_tool_call
    def validate_tool_contract(request, handler):
        name = request.tool_call["name"]
        rejected = rejected_call(request.state.get('messages', []), request.tool_call['id'])
        if rejected is not None:
            failures[name] = failures.get(name, 0) + 1
            if failures[name] > RETRY_LIMIT:
                return retry_limit(request.tool_call['id'], name, "工具参数格式连续校正失败，相关操作未执行，已有成果已保留。")
            return invalid_call_result(request.tool_call['id'], rejected)
        validator = validators.get(name)
        issues = argument_issues(validator, request.tool_call["args"]) if validator else []
        if issues:
            failures[name] = failures.get(name, 0) + 1
            if failures[name] > RETRY_LIMIT:
                return retry_limit(request.tool_call['id'], name, "多次生成的参数都不符合格式要求，本次操作未写入。")
            return ToolMessage(tool_call_id=request.tool_call["id"], name=name, status="error",
                content=json.dumps({"ok": False, "error": {"code": "invalid_arguments", "retryable": True,
                    "message": "本次工具未执行。请依据字段约束修正参数后重新调用，不要让作者处理内部字段类型。", "issues": issues}}, ensure_ascii=False))
        try:
            result = handler(request)
        except RoleOutputError as error:
            return ToolMessage(tool_call_id=request.tool_call['id'],name=name,status='error',content=json.dumps({
                'ok':False,'error':{'code':error.code,'message':str(error),'retryable':False,'outcome':'candidate_preserved'}},ensure_ascii=False))
        except ToolFailure as error:
            failures[name] = failures.get(name, 0) + 1
            if failures[name] > RETRY_LIMIT:
                return retry_limit(request.tool_call['id'], name, "同一工具多次处理失败，已停止重复尝试并保留已有成果和进度。")
            return ToolMessage(tool_call_id=request.tool_call["id"], name=name, status="error",
                               content=json.dumps(error_payload(error.error,error.result),ensure_ascii=False))
        result = normalize_tool_message(result)
        if isinstance(result, ToolMessage) and result.status == "error":
            failures[name] = failures.get(name, 0) + 1
            if failures[name] > RETRY_LIMIT:
                return retry_limit(request.tool_call['id'], name, "同一工具多次处理失败，已停止重复尝试并保留已有成果和进度。")
        else:
            failures.pop(name, None)
        return result

    return validate_tool_contract
