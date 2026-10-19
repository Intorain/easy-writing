//! 正文提交边界：每次提交检查当前稿件和当前规则，工具调用与桌面应用共用。
//! 豁免只接受宿主生成、绑定具体稿件与检查结果的作者点选；模型参数与聊天关键词不构成授权。
use crate::{agent_quality as quality, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

fn content_revision(artifact: &Value) -> Value {
    if artifact["applied"] == true {
        artifact["appliedSourceRevision"]
            .as_u64()
            .map(|v| json!(v))
            .unwrap_or_else(|| artifact["revision"].clone())
    } else {
        artifact["revision"].clone()
    }
}

pub fn report_current(artifact: &Value, report: &Value) -> bool {
    !(report["critic"]["revisedIssueIds"].is_array() && !report["critic"]["revisionAssessment"].is_object())
        && crate::agent_writing_basis::reader_report_is_bound(artifact, report)
        && report["stale"] != true
        && report["reviewedRevision"] == content_revision(artifact)
        && report["reviewedFingerprint"]
            == quality::fingerprint(artifact["content"].as_str().unwrap_or(""))
}

/// 重做确定性检查；同一版本的读者评审可以保留，旧版本的评审不得移植到新稿。
pub async fn refresh_report(
    db: &mut SqliteConnection,
    artifact: &mut Value,
) -> Result<Value, String> {
    let mut report = quality::check_artifact(
        db,
        artifact,
        artifact["content"].as_str().unwrap_or(""),
        str_field(artifact, "id")?,
        content_revision(artifact).as_i64().unwrap_or(0),
        artifact["rulesetId"].as_str(),
    )
    .await?;
    if let Some(fingerprint) = crate::agent_writing_basis::fingerprint(artifact) {
        report["writingBasisFingerprint"] = json!(fingerprint);
    }
    let previous = artifact["reports"]
        .as_array()
        .and_then(|r| r.last())
        .cloned();
    let current = previous
        .as_ref()
        .is_some_and(|r| report_current(artifact, r));
    if let Some(old) = previous.as_ref().filter(|_| current) {
        report["critic"] = old["critic"].clone();
        for issue in old["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["source"] == "critic")
        {
            report["issues"].as_array_mut().unwrap().push(issue.clone());
        }
    }
    report["blocked"] = json!(report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["level"] == "block"));
    if !artifact["reports"].is_array() {
        artifact["reports"] = json!([]);
    }
    let reports = artifact["reports"].as_array_mut().unwrap();
    if current {
        reports.pop();
    }
    for old in reports.iter_mut() {
        old["stale"] = json!(true);
    }
    reports.push(report.clone());
    artifact["textFingerprint"] = report["reviewedFingerprint"].clone();
    Ok(report)
}

fn scope(artifact: &Value, report: &Value) -> Value {
    json!({"artifactId":artifact["id"],"revision":content_revision(artifact),"fingerprint":report["reviewedFingerprint"],
        "bookId":artifact["bookId"],"chapterId":artifact["chapterId"],"expectedVersion":artifact["expectedVersion"],
        "issues":report["issues"]})
}

pub fn waiver_current(artifact: &Value, report: &Value) -> bool {
    artifact["qualityWaiver"]["scope"] == scope(artifact, report)
        && artifact["qualityWaiver"]["approved"] == true
}

fn blocked(report: &Value) -> Value {
    json!({"ok":false,"saved":false,"toolError":{"code":"gate_blocked","retryable":true,
        "message":"这份稿子有程序核出的问题（正文为空或截断、整段重复、作者敏感词、与参考书逐字重合），还不能写入；先处理这些问题。作者明确要保留时可请求确认保存这一版本。",
        "summary":quality::summary(report)}})
}

/// 返回 None 才允许写入。报告与确认记录必须和正文提交在同一事务内处理。
pub async fn check(
    db: &mut SqliteConnection,
    artifact: &mut Value,
    session: Option<&Value>,
    request_waiver: bool,
) -> Result<Option<Value>, String> {
    if artifact["partial"] == true {
        return Err("候选尚未生成完成".into());
    }
    if artifact["pendingRevision"].is_object() {
        return Ok(Some(json!({"ok":false,"saved":false,"toolError":{"code":"revision_pending","retryable":false,
            "message":"当前修订尚未完成验证，不能覆盖正式正文；请完成处理或恢复上一份有效稿件。"}})));
    }
    crate::agent_batch::verify_candidate_sources(db, artifact).await?;
    crate::agent_writing_basis::verify(db, artifact).await?;
    let report = refresh_report(db, artifact).await?;
    if report["blocked"] != true || waiver_current(artifact, &report) {
        return Ok(None);
    }
    let Some(session) = session.filter(|_| request_waiver) else {
        return Ok(Some(blocked(&report)));
    };
    let scope = scope(artifact, &report);
    let task = session["taskId"]
        .as_str()
        .or(session["runId"].as_str())
        .unwrap_or("");
    let key = format!(
        "quality-consent:{}:{}:{}",
        str_field(session, "id")?,
        task,
        quality::fingerprint(&scope.to_string())
    );
    let record = get(db, OP_NS, &key)
        .await?
        .filter(|r| r["declinedRunId"].is_null() || r["declinedRunId"] == session["runId"]);
    if let Some(record) = &record {
        let answer = crate::agent_prompt::confirmation_answer(
            session,
            record["token"].as_str().unwrap_or(""),
        );
        if let Some((picked, _)) = answer {
            if picked.as_deref() == Some("1") {
                artifact["qualityWaiver"] =
                    json!({"approved":true,"scope":scope,"token":record["token"]});
                return Ok(None);
            }
            let mut declined = record.clone();
            declined["declinedRunId"] = session["runId"].clone();
            put(db, OP_NS, &key, &declined).await?;
            return Ok(Some(blocked(&report)));
        }
    }
    let record =
        record.unwrap_or_else(|| json!({"token":id(),"sessionId":session["id"],"scope":scope}));
    put(db, OP_NS, &key, &record).await?;
    let issues = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["level"] == "block")
        .filter_map(|i| i["message"].as_str())
        .collect::<Vec<_>>()
        .join("；");
    Ok(Some(
        json!({"ok":false,"saved":false,"status":"confirmation_required","confirmation":{
        "token":record["token"],"question":format!("「{}」当前稿件仍有这些问题：{}。是否保留问题并保存这一版本？",artifact["title"].as_str().unwrap_or("章节候选"),issues),
        "choices":["先修订，不保存","保留上述问题，保存这一版本"]}}),
    ))
}
