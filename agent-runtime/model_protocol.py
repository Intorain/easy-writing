"""Canonical tool-call envelopes at the model/graph boundary.

Malformed JSON is a model correction task, never an executable operation or an
author question. The same normalization handles new responses and old saved
histories. Existing tool results, message ids and author text are preserved.
"""
import json
from dataclasses import replace
from hashlib import sha256

from langchain.agents.middleware import wrap_model_call
from langchain_core.messages import AIMessage, ToolMessage

REJECTED_CALLS = "ew_rejected_tool_calls"


def normalize_calls(message):
    if not isinstance(message, AIMessage) or not message.invalid_tool_calls:
        return message
    calls = list(message.tool_calls)
    rejected = dict(message.additional_kwargs.get(REJECTED_CALLS) or {})
    used = {call['id'] for call in calls}
    for index, invalid in enumerate(message.invalid_tool_calls):
        call_id = invalid.get('id')
        if not call_id or call_id in used:
            digest = sha256(json.dumps([message.id, index, invalid], sort_keys=True, ensure_ascii=False).encode()).hexdigest()[:24]
            call_id = f'invalid-{digest}'
        used.add(call_id)
        name = invalid.get('name') or 'invalid_tool_call'
        rejected[call_id] = {'name': name, 'arguments': invalid.get('args')}
        # ToolNode routes this envelope through contract_middleware, which
        # returns an error receipt before any handler/side effect can run.
        calls.append({'id': call_id, 'name': name, 'args': {}, 'type': 'tool_call'})
    extra = {key: value for key, value in message.additional_kwargs.items() if key != 'tool_calls'}
    extra[REJECTED_CALLS] = rejected
    return message.model_copy(update={'tool_calls': calls, 'invalid_tool_calls': [], 'additional_kwargs': extra})


def rejected_call(messages, call_id):
    for message in reversed(messages):
        if isinstance(message, AIMessage) and any(call['id'] == call_id for call in message.tool_calls):
            return (message.additional_kwargs.get(REJECTED_CALLS) or {}).get(call_id)
    return None


def invalid_call_result(call_id, rejected):
    return ToolMessage(tool_call_id=call_id, name=rejected['name'], status='error',
        content=json.dumps({'ok': False, 'error': {'code': 'invalid_tool_json', 'retryable': True,
            'message': '本次工具未执行：参数不是合法的 JSON 对象。依据工具契约重新构造参数并调用，不要让作者重新确认已给出的创作要求。',
            'raw_arguments': rejected.get('arguments')}}, ensure_ascii=False))


def repair_history(messages):
    """Supply only missing error receipts for calls known to be unexecutable.

    Older workers persisted invalid_tool_calls alongside an injected question.
    Fill that missing result in its original tool-response group, not at the end
    of the conversation. Never invent success for an unresolved valid call.
    """
    repaired, missing = [], {}
    for original in messages:
        if not isinstance(original, ToolMessage):
            repaired.extend(invalid_call_result(call_id, rejected) for call_id, rejected in missing.items())
            missing = {}
        message = normalize_calls(original)
        repaired.append(message)
        if isinstance(message, AIMessage):
            missing = dict(message.additional_kwargs.get(REJECTED_CALLS) or {})
        elif isinstance(message, ToolMessage):
            missing.pop(message.tool_call_id, None)
    repaired.extend(invalid_call_result(call_id, rejected) for call_id, rejected in missing.items())
    return repaired


@wrap_model_call
def model_protocol(request, handler):
    response = handler(request.override(messages=repair_history(request.messages)))
    return replace(response, result=[normalize_calls(message) for message in response.result])
