"""Declarative semantic tool contracts, exported with the argument schemas.

Resource identities are resolved by kind, never by searching error prose. A field
not listed here stays an opaque domain value; adding an id argument requires an
explicit classification, enforced by contract generation.
"""
from copy import deepcopy

RESOURCES = {
    'book_id': 'book', 'chapter_id': 'chapter', 'as_of_chapter_id': 'chapter',
    'volume_id': 'volume', 'artifact_id': 'artifact', 'plan_id': 'artifact',
    'plan_artifact_id': 'artifact', 'ruleset_id': 'ruleset',
    'distillation_id': 'distillation', 'skill_id': 'skill', 'style_skill_id': 'skill', 'target_skill_id': 'skill', 'project_id': 'breakdown',
    'material_id': 'material', 'profile_id': 'profile', 'entity_id': 'material_entity',
    'thread_id': 'story_thread', 'project_ids':'breakdown', 'evidence_ids':'edit_evidence',
}
# These are checked by their owning domain (not interchangeable record IDs).
DOMAIN_IDS = {'source_id', 'character_id', 'issue_ids'}
# Per-tool resource overrides: open_book brings a book into the conversation, so its
# target is any live book in the library rather than one already in session scope.
TOOL_RESOURCES = {'open_book': {'book_id': 'library_book'}}
SOURCE_BOOK_TOOLS = {'read_chapter', 'list_chapters', 'search_book', 'read_book_materials'}
ARTIFACT_KINDS = {
    'adopt_book_plan': {'artifact_id':['book_plan']},
    'prepare_book_plan': {'plan_id':['book_plan']},
    'prepare_skill': {'artifact_id':['skill']},
    'draft_chapter': {'plan_artifact_id':['scenes','beats']},
    'draft_short_story': {'plan_artifact_id':['structure']},
    'check_chapter': {'artifact_id':['chapter','short']},
    'revise_spans': {'artifact_id':['chapter','short']},
    'polish_draft': {'artifact_id':['chapter','short']},
    'save_chapter': {'artifact_id':['chapter']},
}
NATIVE_ALIASES = {
    'draft_begin':'draft_chapter', 'short_begin':'draft_short_story',
    'delivery_begin':'polish_draft', 'revise_begin':'revise_spans',
    'book_plan_adopt_begin':'adopt_book_plan', 'book_plan_adopt_finish':'adopt_book_plan',
    'breakdown_report_data':'write_breakdown_report','save_breakdown_report':'write_breakdown_report',
}
# Internal stages use the same reference definitions but their own argument shape.
INTERNAL_REFERENCES = {
    'prepare_prose_task': {'artifact_id': 'artifact'},
    'bind_prose_task': {'artifact_id': 'artifact'},
    'draft_finish': {'artifact_id': 'artifact'},
    'artifact_replace': {'artifact_id': 'artifact'},
    'attach_critic': {'artifact_id': 'artifact'},
    'record_critic_failure': {'artifact_id': 'artifact'},
    'draft_scene': {'artifact_id': 'artifact'},
    'delivery_progress': {'artifact_id': 'artifact'},
    'delivery_restore': {'artifact_id': 'artifact'},
    }

NESTED_REFERENCES = {
    'record_book_memory': [
        {'path':'/source_ref/bookId','resource':'book','sameOwner':True},
        {'path':'/source_ref/chapterId','resource':'chapter'},
        {'path':'/source_ref/artifactId','resource':'artifact'},
        {'path':'/source_ref/entityId','resource':'material_entity','integer':True,'materialKindPath':'/source_ref/materialKind'},
        {'path':'/source_ref/sessionId','resource':'author_session'},
        {'path':'/source_ref/messageId','resource':'author_message'},
        {'path':'/supersedes/*/id','resource':'memory'},
    ],
}

# The owner, rather than the presence of a call-id field, defines replay semantics.
# Domain-owned operations already bind revisions/author-confirmation nonces and
# persist request+result atomically; wrapping them in a second claim breaks recovery.
DOMAIN_OPERATIONS = {
    'prepare_distillation','polish_draft','prepare_book_plan','prepare_skill','save_chapter','record_book_memory',
    'start_breakdown','write_breakdown_report','open_chapter_candidate','save_material',
    # 作品管理与通道工具：宿主按操作编号保存请求与结果，重复调用返回同一结果
    'update_volume','update_chapter','update_book','delete_chapter','delete_volume','delete_book',
    'add_inspiration','send_chapter_file','send_rank_chart','generate_cover',
    # 工作备注：宿主按会话+操作编号去重
    'work_note',
    # 自动任务：建立经确认卡，宿主按会话+操作编号留回执；删除按任务编号
    'create_automation','delete_automation',
}
QUERY_OPERATIONS = {'list_rank_sources','get_rank','get_rank_collection'}
BOUNDARY_OPERATIONS = {
    'save_selection_cards','create_volume','create_chapter','update_thread',
    'propose_writing_preference','save_rule','record_suspect_verdict','save_breakdown_recipe',
    'save_comparison','plan_scenes','plan_chapters','draft_chapter','plan_short_story',
    'draft_short_story','save_name_candidates','save_book_intro','request_cover','revise_spans','save_artifact',
}


def build_policies(native):
    policies = {}
    for name, schema in native.items():
        refs = []
        for field in schema.get('properties', {}):
            if field == 'operation_id':
                continue
            if field in RESOURCES:
                resource = TOOL_RESOURCES.get(name, {}).get(field, RESOURCES[field])
                if name in SOURCE_BOOK_TOOLS and resource in ('book','chapter'):
                    resource = 'source_' + resource
                ref={'path':'/'+field+('/*' if field.endswith('_ids') else ''),'resource':resource}
                if field=='entity_id':ref['integer']=True
                kinds=ARTIFACT_KINDS.get(name,{}).get(field)
                if kinds:ref['kinds']=kinds
                if name in ('draft_chapter','draft_short_story') and field=='plan_artifact_id':
                    ref['sameSession']=True
                    ref['dependencies']=[{'path':'/rulesetId','resource':'ruleset','recoveryTool':'resolve_ruleset'}]
                    if name=='draft_chapter':ref['matches']={'chapterId':'/chapter_id'}
                refs.append(ref)
            elif field.endswith(('_id', '_ids')) and field not in DOMAIN_IDS:
                raise ValueError(f'{name}.{field} needs a resource or domain identity policy')
        refs.extend(deepcopy(NESTED_REFERENCES.get(name, [])))
        if name in ('plan_scenes','plan_chapters','plan_short_story'):
            refs.append({'path':'/plan/rulesetId','resource':'ruleset'})
            if name == 'plan_short_story':
                refs.append({'path':'/plan/styleSampleIds/*','resource':'style_example'})
        has_operation = 'operation_id' in schema.get('properties',{})
        if has_operation and name not in DOMAIN_OPERATIONS | QUERY_OPERATIONS | BOUNDARY_OPERATIONS:
            raise ValueError(f'{name} needs an explicit operation receipt owner')
        if name in ('check_chapter','revise_spans'):
            for reference in refs:
                if reference['resource']=='artifact':
                    reference['stableBasis']=True
                    reference['dependencies']=[{'path':'/rulesetId','resource':'ruleset','recoveryTool':'polish_draft'}]
        policies[name]={'references':refs,'bindOperation':has_operation and name in BOUNDARY_OPERATIONS,
                        'effect':'write' if (has_operation and name not in QUERY_OPERATIONS) or name in ('apply_creation_artifact','configure_book_writing','select_write_target') else 'read',
                        'resultKind':'delivery' if name in ('polish_draft','draft_chapter','draft_short_story','revise_spans') else 'value',
                        'receiptOwner':'boundary' if name in BOUNDARY_OPERATIONS else 'domain' if name in DOMAIN_OPERATIONS else 'none'}
        if name in QUERY_OPERATIONS:
            policies[name]['failureStatuses'] = {
                'invalid_request': {'code':'invalid_arguments','category':'arguments','retryable':True,'outcome':'not_applied','recoveryTool':'list_rank_sources'},
                'unavailable': {'code':'source_unavailable','category':'source','retryable':False,'outcome':'not_applied','recoveryTool':'list_rank_sources'},
                'cancelled': {'code':'cancelled','category':'operation','retryable':False,'outcome':'not_applied'},
            }
    for internal, public in NATIVE_ALIASES.items():
        policies[internal]=deepcopy(policies[public])
        policies[internal]['resultKind']='value'
    for stage in ('revise_begin','delivery_begin','book_plan_adopt_begin','book_plan_adopt_finish'):
        policies[stage]['bindOperation']=False
    # Adoption's begin/finish legitimately share an operation id across stages.
    for name, fields in INTERNAL_REFERENCES.items():
        policies[name]={'references':[{'path':'/'+f,'resource':r,'kinds':['chapter','short']} for f,r in fields.items()],
                        'bindOperation':name in ('draft_finish','artifact_replace'), 'effect':'write'}
        if name in ('draft_finish','artifact_replace'):
            policies[name]['references'][0]['dependencies']=[{'path':'/rulesetId','resource':'ruleset','recoveryTool':'polish_draft'}]
    return policies
