//! Artifact identity does not grant access. Cross-session reads inherit the
//! associated book's existing access scope; independent artifacts stay private.
use crate::agent_store::*;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

pub async fn read(db: &mut SqliteConnection, session: &Value, aid: &str) -> Result<Value, String> {
    let artifact = get(db, NS, &format!("artifact:{aid}"))
        .await?
        .filter(|a| !a["deletedAt"].is_number())
        .ok_or("成果不存在或已删除")?;
    if artifact["sessionId"] != session["id"] {
        let bid = artifact["bookId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("该独立成果不属于当前会话")?;
        crate::agent_service::allowed_book(db, session, bid, false).await?;
    }
    Ok(crate::agent_book_plan::with_progress(artifact))
}

/// 交给规划模型的成果视图。正文、计划、交付状态、读者感受和最近一次体检照常给；
/// 写作依据（规则全文、文风技能、受保护资料）、评审输出的原始记录、与当前正文相同的旧版本、
/// 宿主核对用的来源证明只留摘要——这些由宿主在起草、体检、修订时直接取用，规划模型不必每次搬一遍。
/// 库里的记录不变，界面与宿主内部仍读完整成果。
pub fn model_view(artifact: &Value) -> Value {
    let mut out = artifact.clone();
    let Some(fields) = out.as_object_mut() else { return out };
    let content = artifact["content"].as_str();
    for key in ["generatedContent", "initialContent", "original"] {
        if artifact[key].as_str().is_some_and(|text| text.is_empty() || Some(text) == content) {
            fields.remove(key);
        }
    }
    let basis = &artifact["writingBasis"];
    if basis.is_object() {
        fields.insert("writingBasis".into(), json!({
            "rulesetId": basis["rulesetId"],
            "fingerprint": basis["fingerprint"],
            "styleSkill": basis["styleSkill"]["name"],
            "chapterPlan": basis["chapterPlan"].is_string(),
            "sources": basis["sources"].as_array().map_or(0, Vec::len),
            "styleExamples": basis["style"].as_array().map_or(0, Vec::len),
            "note": "写作依据由宿主保存，起草、体检、修订时自动交给写作与评审"
        }));
    }
    if let Some(failures) = artifact["reviewFailures"].as_array() {
        fields.insert("reviewFailures".into(), json!({"count": failures.len(),
            "note": "评审输出的原始记录留在成果里供排查；原因见最近一次体检的 critic"}));
    }
    if let Some(reports) = artifact["reports"].as_array().filter(|r| r.len() > 1) {
        fields.insert("reportCount".into(), json!(reports.len()));
        fields.insert("reports".into(), json!([reports.last()]));
    }
    fields.remove("sourceContext");
    out
}
