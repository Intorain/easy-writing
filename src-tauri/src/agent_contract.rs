//! 与模型共用生成的 JSON Schema。格式不正确的调用在打开写事务前返回可修正错误。
use serde_json::{json, Value};
use std::{collections::HashMap, sync::OnceLock};

struct Contracts {
    fields: HashMap<String, jsonschema::Validator>,
    tools: HashMap<String, jsonschema::Validator>,
    results: HashMap<String, jsonschema::Validator>,
    /// 作品状态层文档（章节卡、拍子、规则集、质量报告、平台画像、引导交互等）。
    documents: HashMap<String, jsonschema::Validator>,
    definitions: Value,
    book_plan: jsonschema::Validator,
}
static CONTRACTS: OnceLock<Result<Contracts, String>> = OnceLock::new();
fn contracts() -> Result<&'static Contracts, String> {
    CONTRACTS
        .get_or_init(|| {
            let definitions: Value =
                serde_json::from_str(include_str!("../../agent-runtime/tool-contracts.json"))
                    .map_err(|e| e.to_string())?;
            let compile =
                |section: &str| -> Result<HashMap<String, jsonschema::Validator>, String> {
                    definitions[section]
                        .as_object()
                        .ok_or("工具契约格式错误")?
                        .iter()
                        .map(|(name, schema)| {
                            Ok((
                                name.clone(),
                                jsonschema::validator_for(schema).map_err(|e| e.to_string())?,
                            ))
                        })
                        .collect()
                };
            Ok(Contracts {
                fields: compile("materialFields")?,
                tools: compile("native")?,
                results: compile("results")?,
                // 旧版契约文件没有 documents 段时按空表处理，宿主仍可启动；调用文档校验会明确报未知类型。
                documents: if definitions["documents"].is_object() {
                    compile("documents")?
                } else {
                    HashMap::new()
                },
                book_plan: jsonschema::validator_for(&definitions["bookPlan"])
                    .map_err(|e| e.to_string())?,
                definitions,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Host-only semantics share the same generated source as model argument schemas.
pub fn tool_policy(name: &str) -> Result<Value, String> {
    contracts()?.definitions["toolPolicies"].get(name).filter(|p|p.is_object()).cloned()
        .ok_or_else(||format!("工具缺少执行策略：{name}"))
}

pub fn validate_result(name:&str,value:&Value)->Result<(),String> {
    if value["toolError"].is_object() || value["ok"]==false || value["status"]=="confirmation_required" {return Ok(());}
    if let Some(schema)=contracts()?.results.get(name) {
        schema.validate(value).map_err(|_|format!("工具 {name} 的返回值不符合共享契约，不能猜测操作结果"))?;
    }
    Ok(())
}

/// JSON 的整数允许写成 1.0；在边界统一为整数，业务层不再各自猜测类型。
pub fn canonical_arguments(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_arguments).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), canonical_arguments(v)))
                .collect(),
        ),
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap();
            if f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0 {
                json!(f as i64)
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

pub fn tool_error(name: &str, args: &Value) -> Result<Option<Value>, String> {
    let Some(validator) = contracts()?.tools.get(name) else {
        return Ok(None);
    };
    let issues=validator.iter_errors(args).take(8).map(|e|json!({"field":e.instance_path.to_string(),"message":e.to_string().chars().take(240).collect::<String>()})).collect::<Vec<_>>();
    if issues.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        json!({"ok":false,"toolError":{"code":"invalid_arguments","retryable":true,
        "message":"工具参数不符合契约，本次未执行写入。请修正字段后重新调用。","issues":issues}}),
    ))
}

pub fn validate_material(kind: &str, fields: &Value) -> Result<(), String> {
    let contracts = contracts()?;
    let validator = contracts.fields.get(kind).ok_or("资料类型无效")?;
    if let Some(error) = validator.iter_errors(fields).next() {
        let key = error
            .instance_path
            .to_string()
            .split('/')
            .nth(1)
            .unwrap_or("")
            .to_owned();
        let definition = &contracts.definitions["materialFields"][kind]["properties"][&key];
        let label = definition["title"].as_str().unwrap_or("资料字段");
        let requirement = match definition["type"].as_str() {
            Some("string") => "文字",
            Some("integer") => "范围内的整数",
            Some("array") => "文字列表",
            _ => "规定格式",
        };
        return Err(format!("{label}需要填写{requirement}，当前内容未保存"));
    }
    Ok(())
}

pub fn validate_book_plan(plan: &Value) -> Result<(), String> {
    contracts()?
        .book_plan
        .validate(plan)
        .map_err(|_| "开书草案格式不正确，请填写工作书名并检查内容".to_owned())
}

/// 契约里声明的文档类型名（chapterCard、qualityReport 等）。
#[cfg(test)]
pub fn document_kinds() -> Result<Vec<String>, String> {
    let mut kinds = contracts()?.documents.keys().cloned().collect::<Vec<_>>();
    kinds.sort();
    Ok(kinds)
}

/// 用与模型侧相同的 Schema 校验文档；调用方先经过 `canonical_arguments`，1.0 这类整数按整数处理。
/// 返回带字段路径的问题列表，空表示通过。
pub fn document_issues(kind: &str, value: &Value) -> Result<Vec<Value>, String> {
    let validator = contracts()?
        .documents
        .get(kind)
        .ok_or_else(|| format!("未知文档类型：{kind}"))?;
    Ok(validator
        .iter_errors(value)
        .take(8)
        .map(|e| {
            json!({"field":e.instance_path.to_string(),"message":e.to_string().chars().take(240).collect::<String>()})
        })
        .collect())
}

pub fn validate_document(kind: &str, value: &Value) -> Result<(), String> {
    let issues = document_issues(kind, value)?;
    if issues.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{kind} 不符合契约：{}",
        issues
            .iter()
            .map(|i| format!(
                "{} {}",
                i["field"].as_str().unwrap_or("$"),
                i["message"].as_str().unwrap_or("")
            ))
            .collect::<Vec<_>>()
            .join("；")
    ))
}

/// Derived analysis accepts only declared fields. Extra model commentary is not
/// an executable argument; discard it before validating required values. Tool
/// requests and author approvals retain their existing strict validation.
pub fn project_document(kind: &str, value: &Value) -> Result<Value, String> {
    fn project(value: &Value, schema: &Value, root: &Value) -> Value {
        if let Some(reference)=schema["$ref"].as_str().and_then(|r|r.strip_prefix('#')) {
            if let Some(target)=root.pointer(reference) {return project(value,target,root);}
        }
        match value {
            Value::Object(fields) if schema["type"]=="object" => Value::Object(fields.iter()
                .filter(|(key,_)|schema["additionalProperties"]!=false || schema["properties"].get(*key).is_some())
                .map(|(key,value)|(key.clone(),project(value,&schema["properties"][key],root))).collect()),
            Value::Array(items) if schema["type"]=="array" => Value::Array(items.iter().map(|v|project(v,&schema["items"],root)).collect()),
            _=>value.clone(),
        }
    }
    let schema=contracts()?.definitions["documents"].get(kind).ok_or("未知文档类型")?;
    Ok(project(value,schema,schema))
}
