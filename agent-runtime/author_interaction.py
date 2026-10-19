"""Author-facing decisions and bounded interpretation of version-bound adoption requests."""
import json
from typing import Literal
from pydantic import BaseModel
from langchain_core.messages import HumanMessage, SystemMessage
from structured_output import invoke_structured


class AdoptionIntent(BaseModel):
    decision: Literal['adopt', 'decline', 'unclear']


ADOPTION_RULES = '''解释作者本轮答复是否授权采用 plan 指向的开书草案，不执行写作任务。
平台已核对该草案属于当前会话，且该版本在本轮答复前展示过；版本、来源与写入校验由平台负责，你只判断本轮答复的语义。
把 precedingConversation / previousReply 作为对话上下文，用 authorText 决定当前授权。先解析答复承接的动作与对象，再分类，不能按孤立短语分类。
adopt：本轮直接要求采用目标草案保存成作品，或肯定回应了前文针对这个草案的单一采用问题。肯定回应承接所问的动作，不需要复述书名、保存动词或整条请求。
decline：本轮没有授权采用，包括否定、尚未满足的保存条件、仅授权其他动作、仅引用或转述他人的保存要求。对试写、取名、讨论的同意只授权对应动作。
unclear：结合完整上下文后，仍有多个不同的采用对象或动作解释，无法确定本轮授权。
助手提出的建议本身不是授权；只有作者当前答复作出选择才产生授权。保留原话中的否定和条件。只返回决定，平台自行绑定完整作者原话。'''


def interpret_adoption(model, context):
    if not context.get('authorText'):
        return {'decision': 'unclear', 'author_quote': ''}
    context = {key: context.get(key) for key in ('authorText', 'plan', 'previousReply', 'precedingConversation', 'otherPlans')}
    context['otherPlans'] = [{key: p.get(key) for key in ('id', 'revision', 'title')} for p in context.get('otherPlans') or []]
    result = invoke_structured(model, AdoptionIntent, [
        SystemMessage(content=ADOPTION_RULES), HumanMessage(content=json.dumps(context, ensure_ascii=False))])
    return {**result['parsed'].model_dump(), 'author_quote': context['authorText'],
            '_usage': getattr(result.get('raw'), 'usage_metadata', None) or {}}
