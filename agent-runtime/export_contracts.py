"""Generate the host/runtime contract from the actual model-facing tool definitions.

同时从同一份 Schema 生成前端 `src/types/canon.ts`。`--check` 校验两份产物都与实现一致。
"""
import argparse
import copy
import json
from pathlib import Path
from contracts import material_schemas, document_schemas, BookPlan, RESULT_TYPES
from pydantic import TypeAdapter
from tool_policies import build_policies

CONTRACT_PATH = Path(__file__).with_name("tool-contracts.json")
TS_PATH = Path(__file__).resolve().parents[1] / "src" / "types" / "canon.ts"
MATERIAL_TYPE_NAMES = {"character": "CharacterFields", "outline": "OutlineFields", "world_setting": "WorldFields"}


def generate():
    from worker import DOMAIN_TOOLS, LOCAL_TOOLS
    fields = material_schemas()
    model, native = {}, {}
    for tool in DOMAIN_TOOLS:
        if tool.name in LOCAL_TOOLS:
            continue
        schema = tool.tool_call_schema.model_json_schema()
        schema["additionalProperties"] = False
        if tool.name == "save_material":
            schema["allOf"] = [{"if": {"properties": {"material_kind": {"const": kind}}},
                                "then": {"properties": {"fields": field_schema}}}
                               for kind, field_schema in fields.items()]
            # 新建条目必须有名称；修改条目可以只提交能力等实际改动字段。
            schema["allOf"] += [{"if": {"properties": {"material_kind": {"const": kind},
                                                        "entity_id": {"type": "null"}}},
                                 "then": {"properties": {"fields": {"required": ["title" if kind == "outline" else "name"]}}}}
                                for kind in fields]
        if tool.name == "save_name_candidates":
            schema["allOf"] = [{"if": {"properties": {"kind": {"const": "书名"}}, "required": ["kind"]},
                                "then": {"properties": {"candidates": {"items": {
                                    "required": ["readerPromise", "storyBasis", "tradeoff"]}}}}}]
        model[tool.name] = schema
        rpc = copy.deepcopy(schema)
        if tool.name == "save_material":
            # Legacy callers may supply their read version; models never manufacture it.
            rpc["properties"]["expected_version"] = {"type": ["string", "null"]}
        # 注入参数（InjectedToolArg）模型看不到，但 worker 内部调用会带上：进原生契约，不进模型契约
        for name, prop in tool.get_input_schema().model_json_schema().get("properties", {}).items():
            if name != "tool_call_id" and name not in rpc["properties"]:
                rpc["properties"][name] = {key: value for key, value in prop.items() if key in ("type", "anyOf", "items", "enum")}
        if "tool_call_id" in tool.get_input_schema().model_fields:
            rpc["properties"]["operation_id"] = {"type": "string", "minLength": 1}
            rpc.setdefault("required", []).append("operation_id")
        native[tool.name] = rpc
    return {"version": 1, "bookPlan": TypeAdapter(BookPlan).json_schema(), "materialFields": fields,
            "model": model, "native": native, "documents": document_schemas(),
            "results": {name:TypeAdapter(schema).json_schema() for name,schema in RESULT_TYPES.items()},
            "toolPolicies": build_policies(native)}


# ---------------------------------------------------------------------------
# JSON Schema -> TypeScript。只覆盖 pydantic 对 TypedDict / Literal / list / dict 产出的形状。
# ---------------------------------------------------------------------------
TS_HEADER = """// 由 agent-runtime/export_contracts.py --ts 生成，请勿手改。
// 修改 agent-runtime/contracts.py 后运行 `uv run python export_contracts.py --ts`。
// 模型侧、Rust 宿主与前端共用同一份契约。

"""


def _literal(value):
    # 与仓库 TS 风格一致：字符串字面量用单引号。
    if isinstance(value, str):
        return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"
    return json.dumps(value, ensure_ascii=False)


class _Renderer:
    def __init__(self):
        self.blocks: list[str] = []
        self.emitted: dict[str, dict] = {}
        self.defs: dict[str, dict] = {}

    def type_of(self, schema: dict) -> str:
        if "$ref" in schema:
            name = schema["$ref"].split("/")[-1]
            self.emit(name, self.defs[name])
            return name
        if "anyOf" in schema:
            parts = []
            for option in schema["anyOf"]:
                rendered = self.type_of(option)
                if rendered not in parts:
                    parts.append(rendered)
            return " | ".join(parts)
        if "enum" in schema:
            return " | ".join(_literal(v) for v in schema["enum"])
        if "const" in schema:
            return _literal(schema["const"])
        kind = schema.get("type")
        if isinstance(kind, list):
            return " | ".join(self.type_of({**schema, "type": t}) for t in kind)
        if kind == "string":
            return "string"
        if kind in ("integer", "number"):
            return "number"
        if kind == "boolean":
            return "boolean"
        if kind == "null":
            return "null"
        if kind == "array":
            inner = self.type_of(schema.get("items", {}))
            return f"Array<{inner}>" if " | " in inner else f"{inner}[]"
        if kind == "object":
            if "properties" in schema:
                return "{ " + "; ".join(self.members(schema)) + " }"
            extra = schema.get("additionalProperties")
            if isinstance(extra, dict):
                return f"Record<string, {self.type_of(extra)}>"
            return "Record<string, unknown>"
        return "unknown"

    def members(self, schema: dict) -> list[str]:
        required = set(schema.get("required", []))
        lines = []
        for key, prop in schema.get("properties", {}).items():
            optional = "" if key in required else "?"
            lines.append(f"{key}{optional}: {self.type_of(prop)}")
        return lines

    def emit(self, name: str, schema: dict):
        if name in self.emitted:
            if self.emitted[name] != schema:
                raise SystemExit(f"契约中存在同名但不同结构的定义：{name}")
            return
        self.emitted[name] = schema
        doc = " ".join(part for part in (schema.get("title") if schema.get("title") != name else None,
                                          schema.get("description")) if part)
        comment = f"/** {doc} */\n" if doc else ""
        if schema.get("type") == "object" and "properties" in schema:
            body = []
            for key, prop in schema["properties"].items():
                note = prop.get("description") or (prop.get("title") if prop.get("title") not in (None, key.capitalize(), key) else None)
                optional = "" if key in set(schema.get("required", [])) else "?"
                rendered = self.type_of(prop)
                if note:
                    body.append(f"  /** {note} */")
                body.append(f"  {key}{optional}: {rendered}")
            self.blocks.append(f"{comment}export interface {name} {{\n" + "\n".join(body) + "\n}\n")
        else:
            self.blocks.append(f"{comment}export type {name} = {self.type_of(schema)}\n")

    def document(self, name: str, schema: dict):
        local = dict(schema)
        self.defs = {**self.defs, **local.pop("$defs", {})}
        self.emit(name, local)


def render_typescript(contract: dict) -> str:
    renderer = _Renderer()
    renderer.document("BookPlan", contract["bookPlan"])
    for kind, name in MATERIAL_TYPE_NAMES.items():
        schema = {k: v for k, v in contract["materialFields"][kind].items() if k != "minProperties"}
        renderer.document(name, schema)
    kinds = []
    for key, schema in contract["documents"].items():
        renderer.document(schema["title"], schema)
        kinds.append((key, schema["title"]))
    footer = "export type CanonDocumentKind = " + " | ".join(_literal(k) for k, _ in kinds) + "\n\n"
    footer += "export interface CanonDocuments {\n" + "\n".join(f"  {k}: {t}" for k, t in kinds) + "\n}\n\n"
    footer += f"export const CANON_CONTRACT_VERSION = {contract['version']}\n"
    return TS_HEADER + "\n".join(renderer.blocks) + "\n" + footer


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="校验 tool-contracts.json 与 src/types/canon.ts 是否与实现一致")
    parser.add_argument("--ts", action="store_true", help="同时生成 src/types/canon.ts")
    args = parser.parse_args()
    contract = generate()
    rendered = json.dumps(contract, ensure_ascii=False, indent=2) + "\n"
    typescript = render_typescript(contract)
    if args.check:
        if not CONTRACT_PATH.exists() or CONTRACT_PATH.read_text(encoding="utf-8") != rendered:
            raise SystemExit("工具契约与实现不一致，请运行 python export_contracts.py --ts 后再构建")
        if not TS_PATH.exists() or TS_PATH.read_text(encoding="utf-8") != typescript:
            raise SystemExit("前端契约类型与实现不一致，请运行 python export_contracts.py --ts 后再构建")
    else:
        CONTRACT_PATH.write_text(rendered, encoding="utf-8")
        if args.ts:
            TS_PATH.write_text(typescript, encoding="utf-8")
