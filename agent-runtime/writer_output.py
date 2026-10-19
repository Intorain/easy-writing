"""The writer produces prose; tool protocol belongs only to the planner."""
import json
import xml.etree.ElementTree as ET


class RoleOutputError(ValueError):
    code = 'invalid_role_output'


def validate_prose(text):
    value = text.strip()
    if not value:
        raise RoleOutputError('写作模型没有返回正文，候选未完成。')
    try:
        document = json.loads(value)
    except ValueError:
        document = None
    if isinstance(document,dict) and ('tool_calls' in document or 'function_call' in document):
        raise RoleOutputError('写作模型返回了工具协议，未将其作为小说正文提交。')
    if value.startswith('<'):
        try:
            root = ET.fromstring(value)
        except ET.ParseError:
            root = None
        if root is not None and root.tag in ('tool_calls','invoke','function_call'):
            raise RoleOutputError('写作模型返回了工具协议，未将其作为小说正文提交。')
    return text
