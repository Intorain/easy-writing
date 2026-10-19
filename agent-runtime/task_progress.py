"""Detect repeated work from persisted tool receipts, not graph-node counts.

同一操作（参数相同）拿到同样的回执：第 3 次提醒模型换做法，第 5 次收起工具，让模型向作者如实总结后
正常结束，不抛异常让整轮判失败。参数里模型选的成果编号、规则集编号保留——带着新编号重试是另一次操作；
每次调用都不同的操作编号，以及回执里新生成的编号和时间不参与比对。
"""
import json
from hashlib import sha256
from langchain_core.messages import AIMessage, ToolMessage, HumanMessage
from tool_results import failure_of, legacy_write_result

WARN_AT = 3
STOP_AT = 5
VOLATILE_ARGS = {'operation_id', 'operationId'}
VOLATILE_RESULT = {'id', 'artifact_id', 'artifactId', 'rulesetId', 'ruleset_id', 'createdAt', 'updatedAt', 'runId', 'operationId'}


def stable(value, volatile=VOLATILE_RESULT):
    if isinstance(value, dict):
        return {k: stable(v, volatile) for k, v in value.items() if k not in volatile}
    if isinstance(value, list):
        return [stable(v, volatile) for v in value]
    return value


def task_progress(state):
    """返回 None，或 {'level': 'warn'|'stop', 'tool', 'message', 'reply'}。作者的新消息让计数重新开始。

    message 写给模型；reply 是收起工具后模型仍没给说明时，直接回给作者的话。"""
    calls, seen, limited = {}, {}, None
    for message in state.get('messages', []):
        if isinstance(message, HumanMessage):
            calls, seen, limited = {}, {}, None
        elif isinstance(message, AIMessage):
            calls.update({c['id']: c for c in message.tool_calls})
        elif isinstance(message, ToolMessage) and message.tool_call_id in calls:
            call = calls[message.tool_call_id]
            try:
                result = json.loads(message.content)
            except (ValueError, TypeError):
                result = message.content
            # Also normalizes persisted receipts when resuming an older checkpoint.
            error = failure_of(result, tool_name=call['name'], legacy_operation=legacy_write_result(call['name']))
            if error and error.get('code') == 'tool_retry_limit':
                limited = call['name']  # 工具边界已判定这个工具连续失败到上限
                continue
            if error and error.get('code') in ('invalid_arguments', 'invalid_tool_json'):
                continue  # Argument repair has its own bounded contract middleware.
            outcome = {'error': error} if error else result
            signature = sha256(json.dumps([call['name'], stable(call['args'], VOLATILE_ARGS), stable(outcome)],
                sort_keys=True, ensure_ascii=False, default=str).encode()).hexdigest()
            count, _ = seen.get(signature, (0, call['name']))
            seen[signature] = (count + 1, call['name'])
    if limited:
        return {'level': 'stop', 'tool': limited,
                'message': f'{limited} 连续失败到上限，平台已停止重试并收起工具。'
                           '请直接向作者说明：已经完成了什么、卡在哪一步、需要作者做什么；已有成果都已保留。',
                'reply': f'已停止重复尝试：{limited} 多次没有成功，相关操作没有执行，已有成果都已保留。告诉我接下来怎么处理即可。'}
    if not seen:
        return None
    count, name = max(seen.values(), key=lambda item: item[0])
    if count >= STOP_AT:
        return {'level': 'stop', 'tool': name, 'count': count,
                'message': f'同一操作（{name}）已用相同参数重复 {count} 次，结果没有变化，平台已收起工具。'
                           '请直接向作者说明：已经完成了什么、卡在哪一步、需要作者决定什么；已有成果都已保留。',
                'reply': stop_reply(name)}
    if count >= WARN_AT:
        return {'level': 'warn', 'tool': name, 'count': count,
                'message': f'同一操作（{name}）已用相同参数拿到相同结果 {count} 次，再重复不会有新进展。'
                           '换一种做法；做不下去就停下，向作者如实说明完成情况和卡住的原因。'}
    return None


def stop_reply(name):
    """重复到上限、收起工具后模型仍没有给出说明时的兜底回复。"""
    return f'已停止重复操作：{name} 多次返回相同结果，没有新进展。已有成果都已保留，告诉我接下来怎么处理即可。'
