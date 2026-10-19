//! Immutable author provenance and role-specific prose requirements. Model output
//! only classifies source spans; it cannot create authority or new requirements.
//!
//! 任务是作者交代的一件事：从提出到交付，中间的补充、答卡、"继续"都属于同一任务。
//! runId 只代表一次进程执行；任务包含的轮次记在会话的 `taskRunIds`，作者要求、写入目标、
//! 技能选择和任务结果都按任务算，不因换了一轮就作废。
use crate::agent_store::*;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 当前任务包含的轮次。升级前的会话没有轮次表，只认当前这一轮。
pub fn runs(session: &Value) -> Vec<&str> {
    let listed: Vec<&str> = session["taskRunIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if listed.is_empty() {
        session["runId"].as_str().into_iter().collect()
    } else {
        listed
    }
}

/// 这条消息或成果是不是在当前任务的某一轮里产生的
pub fn contains_run(session: &Value, run: &Value) -> bool {
    run.as_str().is_some_and(|run| runs(session).contains(&run))
}

/// 本任务里作者的消息（含答卡），按时间顺序。拆书完成后宿主代发的续跑提示不是作者的话。
pub fn author_messages(session: &Value) -> Vec<&Value> {
    let runs = runs(session);
    session["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| {
            m["role"] == "user"
                && m["source"] != "breakdown"
                && m["runId"].as_str().is_some_and(|run| runs.contains(&run))
        })
        .collect()
}

/// 本任务作者说过的话，按原顺序逐条给出（去重、去空）。
/// `taskInstruction` 是发起这件事的那条消息；升级前的会话只在这里留有原话。
pub fn author_texts(session: &Value) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    let first = session["taskInstruction"].as_str();
    for text in first
        .into_iter()
        .chain(author_messages(session).into_iter().filter_map(|m| m["text"].as_str()))
    {
        if !text.trim().is_empty() && !parts.contains(&text) {
            parts.push(text);
        }
    }
    parts
}

/// 本任务作者说过的全部要求，按原顺序、原文拼接，交给规则集与写作依据。
pub fn instruction(session: &Value) -> String {
    author_texts(session).join("\n")
}

/// 无文字的继续沿用上一次作者请求；再次发送同样的文字仍是一个新请求。
pub fn latest_author_request(session: &Value) -> Option<&str> {
    author_request_at(session, i64::MAX)
}

/// 兼容未记录请求编号的旧稿件，按创建时刻找回当时的作者请求。
pub fn author_request_at(session: &Value, at: i64) -> Option<&str> {
    author_messages(session).into_iter().rev()
        .find(|m| m["text"].as_str().is_some_and(|text| !text.trim().is_empty())
            && m["createdAt"].as_i64().unwrap_or(0) <= at)
        .and_then(|m| m["id"].as_str())
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn bind(a: &mut Value, source: &str, projection: &Value) -> Result<(), String> {
    crate::agent_contract::validate_document("taskProjection", projection)?;
    let segments = projection["segments"].as_array().ok_or("任务分段无效")?;
    let joined = segments
        .iter()
        .filter_map(|s| s["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    if compact(&joined) != compact(source) {
        return Err("任务分段没有逐字完整覆盖原始要求".into());
    }
    let writing = segments
        .iter()
        .filter(|s| s["role"] == "writing")
        .filter_map(|s| s["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let basis = &mut a["writingBasis"];
    let rules = basis["baseRulesText"]
        .as_str()
        .or(basis["rulesText"].as_str())
        .unwrap_or("")
        .to_owned();
    basis["baseRulesText"] = json!(rules);
    basis["taskProjection"] = projection.clone();
    basis["rulesText"] = json!(if writing.is_empty() {
        rules
    } else {
        format!("【作者对本次任务的创作要求（最高优先级）】\n{writing}\n\n{rules}")
    });
    let writer_base = basis["baseWriterRulesText"].as_str().unwrap_or("");
    basis["writerRulesText"] = json!(format!("【作者创作要求】\n{writing}\n\n{writer_base}"));
    basis
        .as_object_mut()
        .ok_or("写作依据无效")?
        .remove("fingerprint");
    basis["fingerprint"] = json!(crate::agent_creation::version(basis));
    for report in a["reports"].as_array_mut().into_iter().flatten() {
        report["stale"] = json!(true);
    }
    Ok(())
}

pub async fn prepare(
    db: &mut SqliteConnection,
    s: &Value,
    args: &Value,
    finish: bool,
) -> Result<Value, String> {
    let aid = str_field(args, "artifact_id")?;
    let mut a = crate::agent_artifacts::read(db, s, aid).await?;
    if a["sessionId"] != s["id"]
        || (!args["expected_revision"].is_null() && a["revision"] != args["expected_revision"])
    {
        return Ok(crate::agent_tool_boundary::failure(
            "stale_artifact",
            crate::agent_tool_boundary::FailureKind::Conflict,
            "候选已变化，未覆盖其任务要求。",
            true,
            "read_artifact",
            json!({}),
        ));
    }
    let rules = crate::agent_ruleset::for_artifact(db, s, &mut a, None).await?;
    if rules["toolError"].is_object() {
        return Ok(rules);
    }
    crate::agent_writing_basis::ensure(db, &mut a).await?;
    if a["writingBasis"].get("authorTask").is_none() {
        let rules = crate::agent_ruleset::load(db, str_field(&a, "rulesetId")?).await?;
        a["writingBasis"]["authorTask"] = rules["authorInstruction"].clone();
        // Legacy bases may contain execution instructions. Rebuild the role-neutral
        // rule view from the bound source, never filter arbitrary prose by keywords.
        a["writingBasis"]["baseRulesText"] = json!(crate::agent_ruleset::expand(&rules));
    }
    let source = a["writingBasis"]["authorTask"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let source_id = crate::agent_creation::version(&json!(source));
    let key = format!("task-projection:{}:{source_id}", str_field(s, "id")?);
    if !a["writingBasis"]["taskProjection"].is_object() {
        let projection = if finish {
            if args["sourceId"] != source_id {
                return Ok(crate::agent_tool_boundary::failure(
                    "task_source_changed",
                    crate::agent_tool_boundary::FailureKind::Conflict,
                    "任务来源已变化，未采用旧分类。",
                    false,
                    "read_artifact",
                    json!({}),
                ));
            }
            Some(args["projection"].clone())
        } else if source.trim().is_empty() {
            Some(json!({"segments":[]}))
        } else {
            get(db, OP_NS, &key).await?
        };
        if let Some(projection) = projection {
            if let Err(message) = bind(&mut a, &source, &projection) {
                return Ok(crate::agent_tool_boundary::failure(
                    "invalid_task_projection",
                    crate::agent_tool_boundary::FailureKind::Precondition,
                    &message,
                    false,
                    "read_artifact",
                    json!({"artifactId":aid}),
                ));
            }
            put(db, OP_NS, &key, &projection).await?;
            put(db, NS, &format!("artifact:{aid}"), &a).await?;
        } else {
            return Ok(json!({"ready":false,"source":source,"sourceId":source_id}));
        }
    }
    Ok(
        json!({"ready":true,"revision":a["revision"],"rules_text":a["writingBasis"]["writerRulesText"].as_str().unwrap_or_else(|| a["writingBasis"]["rulesText"].as_str().unwrap_or("")),"writing_basis":a["writingBasis"]}),
    )
}
