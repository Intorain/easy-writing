"""Resource limits, independent of writing methods and author-intent interpretation.

Configured model capacity is authoritative. Every internal request includes its
messages and output schema in the context estimate; required input is never cut
to make a request fit. Defaults apply only when model settings omit a limit.
"""
import json
from dataclasses import asdict, dataclass
from langchain_core.language_models.base import BaseLanguageModel


@dataclass(frozen=True)
class RuntimeLimits:
    request_timeout_seconds: int = 180
    extraction_attempts: int = 2
    planner_calls: int = 20
    graph_steps: int = 100
    delivery_model_calls: int = 8
    delivery_seconds: int = 600
    review_attempts: int = 2
    revision_attempts: int = 2


RUNTIME_LIMITS = RuntimeLimits()


class ContextBudgetError(ValueError):
    code = "context_budget"

    def __init__(self, input_tokens, output_tokens, context_tokens):
        super().__init__(f"请求预计需要 {input_tokens} 输入 token 和 {output_tokens} 输出 token，"
                         f"超过当前模型的 {context_tokens} token 上下文；本次未调用模型，未截断原文。")


@dataclass(frozen=True)
class ModelLimits:
    context_tokens: int = 32000
    output_tokens: int = 4096

    @classmethod
    def from_config(cls, config):
        defaults = cls()
        values = {}
        for key, field in (("maxContext", "context_tokens"), ("maxOutputTokens", "output_tokens")):
            value = config.get(key)
            if value is None:
                value = getattr(defaults, field)
            if type(value) is not int or value <= 0:
                raise ValueError(f"模型配置 {key} 必须是正整数")
            values[field] = value
        limits = cls(**values)
        if limits.output_tokens >= limits.context_tokens:
            raise ValueError("模型输出上限必须小于上下文上限，才能容纳输入")
        return limits

    @classmethod
    def for_model(cls, model):
        metadata = getattr(model, "metadata", None) or {}
        stored = metadata.get("ew_model_limits")
        if stored:
            return cls(**stored)
        # Also supports SDK models supplied by the local evaluation harness.
        return cls.from_config({"maxOutputTokens": getattr(model, "max_tokens", None)})

    def metadata(self):
        return {"ew_model_limits": asdict(self)}


def estimate_tokens(model, value):
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    if getattr(type(model), "get_token_ids", None) is BaseLanguageModel.get_token_ids:
        return len(text.encode("utf-8"))
    try:
        return len(model.get_token_ids(text))
    except Exception:
        # A conservative byte bound for custom SDK models without a tokenizer, or
        # when the tokenizer table cannot be loaded (offline first run). An
        # estimate must never surface as a provider connection failure.
        # Never use an English-only chars-per-token ratio for Chinese prose.
        return len(text.encode("utf-8"))


def input_tokens(model, messages, schema=None):
    envelope = [{"role": message.type, "content": message.content} for message in messages]
    if schema is not None:
        envelope.append({"tools": [{"type": "function", "function": {"name": schema.get("title", "Result"),
                                                                   "parameters": schema}}]})
    return estimate_tokens(model, envelope)


def fits_context(model, messages, schema=None):
    limits = ModelLimits.for_model(model)
    return input_tokens(model, messages, schema) + limits.output_tokens <= limits.context_tokens


def prepare_request(model, messages, schema=None, *, streaming=False):
    limits = ModelLimits.for_model(model)
    estimated = input_tokens(model, messages, schema)
    if estimated + limits.output_tokens > limits.context_tokens:
        raise ContextBudgetError(estimated, limits.output_tokens, limits.context_tokens)
    return model.model_copy(update={"streaming": streaming, "max_tokens": limits.output_tokens,
                                    "request_timeout": RUNTIME_LIMITS.request_timeout_seconds, "max_retries": 0})
