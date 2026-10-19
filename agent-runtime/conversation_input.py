"""Start a new author turn without resuming a suspended tool invocation.

LangGraph starts from its entry node when given fresh input. Close outstanding
tool calls in the conversation first so the provider receives a valid history.
No interrupt resume value is fabricated and no pending tool is executed.
"""
import json
from langchain_core.messages import AIMessage, ToolMessage, HumanMessage


def new_turn_messages(messages, author_text):
    pending = {}
    for message in messages:
        if isinstance(message, AIMessage):
            for tool in message.tool_calls:
                pending[tool['id']] = tool.get('name', '')
        elif isinstance(message, ToolMessage):
            pending.pop(message.tool_call_id, None)
    closed = [ToolMessage(tool_call_id=call_id, name=name,
                          content=json.dumps({'status':'superseded',
                              'message':'作者已发送新的消息，旧工具节点不再继续。此消息不是问题答案，也不授予旧操作执行权限。并行工具可能已有结果，已完成操作以平台当前数据和回执为准，不要假定全部未执行或重复操作；按新要求重新判断。'}, ensure_ascii=False))
              for call_id, name in pending.items()]
    return [*closed, HumanMessage(content=author_text)]
