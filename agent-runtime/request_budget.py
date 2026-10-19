"""Charge real model requests once, including structured-output retries.

The calling task owns limits and persistence. SDK/transport retries are disabled;
all attempts pass this boundary before a provider request starts.
"""
from contextlib import contextmanager
from contextvars import ContextVar

_budget = ContextVar('model_request_budget', default=None)
_reserved = ContextVar('model_request_reserve', default=0)


class RequestBudgetExceeded(RuntimeError):
    code = 'budget_exhausted'


@contextmanager
def budget_scope(budget, reserve=0):
    token = _budget.set(budget)
    reserved = _reserved.set(reserve)
    try:
        yield
    finally:
        _reserved.reset(reserved)
        _budget.reset(token)


def charge_request():
    budget = _budget.get()
    if budget is not None and (not budget.allows(1+_reserved.get()) or not budget.reserve()):
        raise RequestBudgetExceeded('本次任务的模型请求预算已用完，已保留有效稿件和阶段结果。')


def invoke_model(model, messages, **kwargs):
    charge_request()
    return model.invoke(messages, **kwargs)


def stream_model(model, messages, **kwargs):
    charge_request()
    yield from model.stream(messages, **kwargs)
