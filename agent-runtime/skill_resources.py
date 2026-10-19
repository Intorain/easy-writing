"""One namespace for skill metadata and virtual filesystem references."""
import re
from hashlib import sha256


def virtual_name(skill_id):
    if len(skill_id)<=64 and re.fullmatch(r'[a-z0-9]+(?:-[a-z0-9]+)*',skill_id):
        return skill_id
    return 'skill-'+sha256(skill_id.encode()).hexdigest()[:40]


def manifest_view(draft):
    result={key:draft[key] for key in ('id','name','description','kind','instructions','revision','readOnly','replaces','requirements') if key in draft}
    root='/skills/'+virtual_name(result['id'])
    result['virtualPath']=root+'/SKILL.md'
    result['files']=[]
    for file in draft.get('files',[]):
        item={key:file[key] for key in ('path','editable','characters','textAvailable') if key in file}
        if isinstance(file.get('content'),str):
            item.update(characters=len(file['content']),textAvailable=True)
        if item['path'].startswith('references/') and item.get('textAvailable'):
            item['virtualPath']=root+'/'+item['path']
        result['files'].append(item)
    return result
