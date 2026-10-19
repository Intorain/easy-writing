"""One interpretation of tool outcomes for direct and nested tool calls.
Only declared failure envelopes are recoverable. Transport/storage exceptions
are never guessed to be argument errors from their message text.
"""
import json
from functools import lru_cache
from pathlib import Path
from langchain_core.messages import ToolMessage


@lru_cache(maxsize=1)
def policies():
    return json.loads(Path(__file__).with_name('tool-contracts.json').read_text(encoding='utf-8')).get('toolPolicies', {})


def legacy_write_result(name):
    return policies().get(name, {}).get('effect') == 'write'


def failure_of(value, *, legacy_operation=False, delivery=False, tool_name=None):
    if not isinstance(value, dict):
        return None
    explicit = value.get('toolError')
    if explicit is None and value.get('ok') is False and isinstance(value.get('error'), dict):
        explicit = value['error']
    if explicit is None and value.get('reviewComplete') is False and isinstance(value.get('deliveryError'), dict):
        explicit = value['deliveryError']
    if explicit is not None and not isinstance(explicit, dict):
        raise ValueError('工具错误回执必须是对象')
    if isinstance(explicit, dict):
        if not isinstance(explicit.get('code'), str) or not isinstance(explicit.get('message'), str):
            raise ValueError('工具错误回执缺少 code 或 message')
        return {**explicit, 'retryable': explicit.get('retryable') is True}
    # Domain status meanings belong to the shared contract, not to consumers
    # guessing from the presence of an `error` field (which may be historical).
    status = value.get('status')
    declared = policies().get(tool_name, {}).get('failureStatuses', {}).get(status) if isinstance(status, str) else None
    if declared:
        message = value.get('message') or value.get('error')
        return {**declared, 'message': message if isinstance(message, str) else '工具未完成当前请求'}
    if isinstance(value.get('error'), str) and value['error'] and (value.get('ok') is False or legacy_operation and any(value.get(flag) is False for flag in ('saved','applied','created'))):
        # Legacy handlers can preserve a candidate while a formal write failed.
        # Unknown effect is never treated as permission to repeat the write.
        return {'code':'operation_not_completed','category':'operation','message':value['error'],
                'retryable':False,'outcome':'unknown','recoveryTool':'read_artifact'}
    progress = value.get('delivery')
    if delivery and isinstance(progress, dict) and progress.get('status') in ('failed','needs_attention','budget_exhausted'):
        return {'code':'delivery_'+progress['status'],'category':'delivery','message':progress.get('reason') or '候选尚未完成本次处理',
                'retryable':False,'outcome':'candidate_preserved','recoveryTool':'read_artifact'}
    return None


def error_payload(error, result=None):
    return {**(result if isinstance(result, dict) else {}), 'ok':False, 'error':error}


def normalize_tool_message(message):
    if not isinstance(message, ToolMessage) or not isinstance(message.content, str):
        return message
    try:
        payload = json.loads(message.content)
    except (ValueError, TypeError):
        return message
    error = failure_of(payload, tool_name=message.name, legacy_operation=legacy_write_result(message.name),
                       delivery=policies().get(message.name, {}).get("resultKind") == "delivery")
    if error is None:
        return message
    return message.model_copy(update={'status':'error','content':json.dumps(error_payload(error,payload),ensure_ascii=False)})
