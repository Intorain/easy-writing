"""Safe diagnostics: provider response bodies and credentials never enter persisted messages."""
def model_failure(error):
    from host import ToolFailure
    if isinstance(error, ToolFailure):
        return {key: error.error[key] for key in ('code','message','retryable','category','outcome','recoveryTool') if key in error.error}
    name = type(error).__name__
    if getattr(error,'code',None)=='context_preparation_failed':
        return {'code':error.code,'message':str(error),'retryable':True}
    if getattr(error,'code',None) in ('budget_exhausted','task_stalled','invalid_role_output'):
        return {'code':error.code,'message':str(error),'retryable':False}
    if name in ('ModelCallLimitExceeded','ModelCallLimitExceededError','GraphRecursionError'):
        return {'code':'budget_exhausted','message':'任务运行达到资源预算，已有成果与检查点已保留。','retryable':False}
    status = getattr(error, 'status_code', None)
    if getattr(error, 'code', None) == 'host_operation':
        return {'code':'host_operation','message':str(error)[:180],'retryable':False}
    if getattr(error, 'code', None) == 'context_budget':
        return {'code':error.code,'message':str(error),'retryable':False}
    if 'Timeout' in name:
        return {'code':'timeout','message':'模型请求超时','retryable':True}
    if status in (401,403):
        return {'code':'authentication','message':f'模型服务拒绝访问（HTTP {status}）','retryable':False}
    if status == 429:
        return {'code':'rate_limit','message':'模型服务限流或额度不足（HTTP 429）','retryable':True}
    if status == 400:
        return {'code':'invalid_request','message':'模型服务拒绝了请求格式（HTTP 400）。已有进度已保留；这不表示网络连接失败。','retryable':False}
    if 'Connection' in name:
        return {'code':'connection','message':'无法连接模型服务','retryable':True}
    if getattr(error, 'code', None) in ('output_limit', 'filtered_output'):
        return {'code':error.code,'message':str(error)[:180],'retryable':error.code == 'output_limit'}
    if isinstance(error, ValueError):
        detail = str(error)
        safe = any(detail.startswith(name + ' 结构化结果') for name in ('MemoryBatch','MemoryExtraction','CriticReview','SpanRevision','AdoptionIntent'))
        return {'code':'invalid_output','message':detail[:180] if safe else '模型结果未通过结构化格式校验','retryable':True}
    if isinstance(status, int):
        return {'code':'provider_error','message':f'模型服务返回错误（HTTP {status}）','retryable':status >= 500}
    return {'code':'processing_error','message':f'处理失败（{name}）','retryable':False}


def log_failure(error, payload):
    """Persist stack locations without exception bodies, local variables or provider credentials."""
    import json, sys, traceback
    frames = traceback.extract_tb(error.__traceback__)
    print(json.dumps({'event':'runtime_failure','sessionId':payload.get('sessionId'),
        'type':type(error).__name__,'frames':[{'file':f.filename,'line':f.lineno,'function':f.name} for f in frames[-18:]]},
        ensure_ascii=False), file=sys.stderr, flush=True)
