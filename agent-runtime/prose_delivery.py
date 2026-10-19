"""Draft delivery: one editor review, at most one targeted revision, verified changes, then hand the text back.

The editor finds concrete, quotable problems; a changed draft is provisional until a separate verification checks
its changes against the same writing basis. A revision that does not verify is rolled back and the original is
delivered: unresolved editor opinions stay on the report as internal data, they do not turn the chapter into a task for
the author. Only program-confirmed blocks (empty, truncated, repeated, sensitive, verbatim overlap) keep the draft
back. Resource limits bound work, not style.
"""
import time
import threading
from dataclasses import dataclass
from model_limits import RUNTIME_LIMITS
from runtime_errors import model_failure
from host import ToolFailure
from request_budget import budget_scope, RequestBudgetExceeded


@dataclass
class DeliveryBudget:
    calls_left: int = RUNTIME_LIMITS.delivery_model_calls
    seconds: float = RUNTIME_LIMITS.delivery_seconds
    on_charge: object = None
    calls_used: int = 0

    def __post_init__(self):
        self.deadline = time.monotonic() + self.seconds
        self.lock = threading.Lock()

    def allows(self, calls=1):
        return self.calls_left >= calls and time.monotonic() < self.deadline

    def reserve(self, calls=1):
        with self.lock:
            if not self.allows(calls):
                return False
            self.calls_left -= calls
            self.calls_used += calls
        if self.on_charge:
            self.on_charge()
        return True

    def snapshot(self):
        return {'callsLeft': self.calls_left, 'callsUsed': self.calls_used,
                'secondsLeft': max(0.0, self.deadline - time.monotonic())}

    @classmethod
    def resume(cls, saved):
        if not saved:
            return cls()
        return cls(calls_left=min(RUNTIME_LIMITS.delivery_model_calls, saved['callsLeft']),
                   calls_used=saved.get('callsUsed', 0),
                   seconds=min(RUNTIME_LIMITS.delivery_seconds, saved['secondsLeft']))


def editable_issues(report):
    """编辑点名且定位到段落的问题，加上程序核出的逐字重合：这些交给写作模型定向修订一次。"""
    return [issue for issue in report.get('issues', [])
            if (issue.get('source') == 'critic' or str(issue.get('code', '')).startswith('overlap.'))
            and issue.get('repairMode') in ('prose_only', 'draft_edit')
            and issue.get('quotes') and issue.get('paragraphs') and issue.get('fix')]


def verified(assessment):
    return (isinstance(assessment, dict) and assessment.get('factsPreserved') is True
            and assessment.get('withinScope') is True and assessment.get('progress') == 'improved')


def deliver_draft(finished, check, revise, progress, restore, *, budget=None, review_cost=1):
    if not finished or finished.get('toolError') or not finished.get('artifact_id'):
        return finished
    budget = budget or DeliveryBudget()
    aid, revision = finished['artifact_id'], finished['revision']
    history, rounds, report = [], 0, {}
    phase = 'reviewing'

    def state(status, reason):
        nonlocal revision, report, phase
        phase = status
        delivery = {'status': status, 'revision': revision, 'rounds': rounds, 'reason': reason,
                    'remainingIssueIds': [i['id'] for i in report.get('issues', [])], 'history': list(history),
                    'budget': budget.snapshot()}
        saved = progress(artifact_id=aid, expected_revision=revision, delivery=delivery) or {}
        old_revision = revision
        revision = saved.get('revision', revision)
        if isinstance(saved.get('report'), dict):
            report = {**report, **saved['report']} if revision == old_revision and report.get('reviewedRevision') == revision else saved['report']
        delivery = saved.get('delivery', delivery)
        return {**finished, 'revision': revision, 'delivery': delivery,
                'reviewComplete': saved.get('reviewComplete', report.get('reviewedRevision') == revision and not report.get('stale')
                                  and report.get('critic', {}).get('status') == 'available'),
                'blocked': report.get('blocked', False), 'summary': report.get('summary', {}),
                'issues': report.get('issues', []), 'critic': report.get('critic')}

    def settle(report, note=''):
        """核对与有限修订都做完了：程序核实的阻断留住稿子；其余情况交稿，未解决的编辑意见只留在报告里。"""
        if report.get('blocked'):
            return state('needs_attention', '正文有程序核实的结构或数据问题，稿子保留为草稿，未交付。' + note)
        pending = any(i.get('repairMode') == 'author_decision' for i in report.get('issues', []))
        if pending:
            return state('awaiting_author', '正文已交付；有一处涉及作者已定设定或主线取舍，需要作者决定。' + note)
        return state('ready', '编辑核对完成，正文已交付。' + note)

    def rollback(operation_id, reason):
        nonlocal revision, report
        restored = restore(artifact_id=aid, expected_revision=revision, operation_id=operation_id)
        revision = restored['revision']
        report = restored.get('report', {})
        history.append({'revision': revision, 'action': 'restore', 'reason': reason})
        return settle(report, reason)

    budget.on_charge = lambda: state(phase, '正在处理当前版本，已记录实际模型请求预算。')
    try:
        review_failures, retry_hint = 0, None
        while True:
            if not budget.allows():
                return state('budget_exhausted', '本次处理达到资源预算，编辑核对未完成，稿子保留为草稿。')
            state('reviewing', '正在核对事实连续性、设定与场景要点。')
            with budget_scope(budget):
                report = check(artifact_id=aid, expected_revision=revision, retry_hint=retry_hint)
            if not report or report.get('toolError') or report.get('critic', {}).get('status') != 'available':
                failure = (report or {}).get('critic', {})
                review_failures += 1
                if failure.get('retryable') and review_failures < RUNTIME_LIMITS.review_attempts:
                    retry_hint = failure.get('error') if failure.get('errorCode') == 'invalid_output' else None
                    history.append({'revision': revision, 'action': 'review', 'reason': '评审未取得有效结果，将在剩余预算内重试。'})
                    continue
                return state('failed', '编辑核对未完成，稿子保留为草稿，不按已交付结束。')
            break
        history.append({'revision': revision, 'action': 'review', 'reason': report['critic'].get('summary') or '已核对当前版本。'})
        if report.get('revision_before'):
            # 恢复一次中断的修订：先核验那次改动，核验不过就回到改前稿
            assessment = (report.get('critic') or {}).get('revisionAssessment') or {}
            if not verified(assessment):
                return rollback(report['revision_operation'], '上一次修订未核验通过，已恢复修订前的稿子。')
            return settle(report)
        actionable = editable_issues(report)
        if not actionable:
            return settle(report)
        if not budget.allows(1 + review_cost):
            return settle(report, '修订预算不足，本次未修订。')
        state('revising', '正在按编辑意见定向修订点名的段落。')
        correction = None
        changed = None
        for attempt in range(RUNTIME_LIMITS.revision_attempts):
            try:
                with budget_scope(budget, reserve=review_cost):
                    changed = revise(artifact_id=aid, expected_revision=revision,
                                     issue_ids=[i['id'] for i in actionable], retry_hint=correction)
                break
            except ValueError as error:
                failure = model_failure(error)
                if failure['code'] != 'invalid_output' or attempt + 1 >= RUNTIME_LIMITS.revision_attempts:
                    history.append({'revision': revision, 'action': 'revise', 'reason': failure['message']})
                    return settle(report, '修订未产生合法结果，交付修订前的稿子。')
                if not budget.allows(1):
                    return settle(report, '修订格式校正所需预算不足，交付修订前的稿子。')
                correction = failure['message']
        if not changed or changed.get('toolError'):
            history.append({'revision': revision, 'action': 'revise', 'reason': (changed or {}).get('toolError', {}).get('message') or '修订未成功。'})
            return settle(report, '修订未成功，交付修订前的稿子。')
        if not changed.get('changed'):
            history.append({'revision': revision, 'action': 'revise', 'reason': '本次修订没有产生有效改动。'})
            return settle(report, '修订没有产生改动。')
        revision = changed['revision']
        report = changed.get('report') or {}
        rounds += 1
        history.append({'revision': revision, 'action': 'revise', 'reason': '已按意见修订，正在核验改动。',
                        'issueIds': [i['id'] for i in actionable]})
        with budget_scope(budget):
            report = check(artifact_id=aid, expected_revision=revision)
        assessment = (report.get('critic') or {}).get('revisionAssessment') or {}
        if report.get('critic', {}).get('status') != 'available' or not assessment:
            return rollback(changed['operation_id'], '修订后的改动没有取得核验结论，已恢复修订前的稿子。')
        if not verified(assessment):
            return rollback(changed['operation_id'], '核验未确认改善，或发现事实、范围变化，已恢复修订前的稿子。')
        history.append({'revision': revision, 'action': 'review', 'reason': assessment.get('reason') or '已核验改动。'})
        return settle(report)
    except RequestBudgetExceeded as error:
        return state('budget_exhausted', str(error))
    except ToolFailure as error:
        try:
            state('needs_attention', error.error['message'])
        except Exception:
            pass  # A newer author revision must not be overwritten just to log an error.
        raise
    except Exception as error:
        failure = model_failure(error)
        try:
            return state('failed', failure['message'])
        except Exception:
            return {**finished, 'revision': revision, 'reviewComplete': False, 'deliveryError': failure,
                    'message': '处理已中止，已保存的稿子保留；请读取当前版本后继续。'}
