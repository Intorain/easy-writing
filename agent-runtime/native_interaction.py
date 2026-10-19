"""UI-owned decisions have one owner and end the planner turn after handoff.

The native card's transaction owns its answer. No synthetic author answer is
injected, and a failed tool receipt still lets the planner correct its request.
"""
import json
from dataclasses import replace
from langchain.agents.middleware import AgentMiddleware, hook_config
from langchain_core.messages import AIMessage, ToolMessage
from model_protocol import REJECTED_CALLS


class NativeInteractionMiddleware(AgentMiddleware):
    def __init__(self, tools):
        self.owners = {t.name for t in tools if (getattr(t, 'metadata', None) or {}).get('native_interaction')}

    def wrap_model_call(self, request, handler):
        response = handler(request)
        messages=[]
        for message in response.result:
            if isinstance(message,AIMessage):
                native=[c for c in message.tool_calls if c['name'] in self.owners]
                if native:
                    # A UI decision is a handoff, not a parallel planner question
                    # or mutation. Other work can be reconsidered after the answer.
                    extra={k:v for k,v in message.additional_kwargs.items() if k!='tool_calls'}
                    if REJECTED_CALLS in extra:
                        ids={c['id'] for c in native}
                        extra[REJECTED_CALLS]={key:value for key,value in extra[REJECTED_CALLS].items() if key in ids}
                    message=message.model_copy(update={'tool_calls':native,'invalid_tool_calls':[], 'additional_kwargs':extra})
            messages.append(message)
        return replace(response,result=messages)

    @hook_config(can_jump_to=['end'])
    def before_model(self,state,runtime):
        for message in reversed(state.get('messages',[])):
            if not isinstance(message,ToolMessage):
                break
            if message.name not in self.owners or message.status=='error':
                continue
            try:
                value=json.loads(message.content)
            except (ValueError,TypeError):
                continue
            if value.get('id') and value.get('status') and not value.get('toolError') and value.get('ok') is not False:
                return {'jump_to':'end'}
        return None
