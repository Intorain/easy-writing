//! Versioned edit evidence. Statistics never infer a ban or global author preference.
//! Human-edited candidates are known; later formal-chapter changes remain unknown
//! unless their source is explicit. Semantic proposals always await author approval.
use crate::agent_store::{chapter, get, list, now, put, str_field, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

pub fn mark_author_edit(before: &Value, after: &mut Value) {
    if before["content"] != after["content"] && crate::agent_quality::is_prose(after) {
        after["lastAuthorEdit"] = json!({"before":before["content"],"after":after["content"],
            "beforeRevision":before["revision"],"afterRevision":after["revision"],"origin":"author_edit"});
    }
}

pub async fn learn(db: &mut SqliteConnection, book_id: &str) -> Result<Value, String> {
    let mut collected = 0;
    for artifact in list(db, NS, "artifact:").await? {
        if artifact["bookId"] != book_id
            || artifact["deletedAt"].is_number()
            || artifact["partial"] == true
            || !crate::agent_quality::is_prose(&artifact)
        {
            continue;
        }
        let edit = &artifact["lastAuthorEdit"];
        let (before, after, origin, revision) = if edit["afterRevision"] == artifact["revision"]
            && edit["after"] == artifact["content"]
            && edit["origin"] == "author_edit"
        {
            (
                edit["before"].as_str().unwrap_or("").to_owned(),
                edit["after"].as_str().unwrap_or("").to_owned(),
                "author_edit",
                artifact["revision"].clone(),
            )
        } else if artifact["applied"] == true {
            let Some(cid) = artifact["chapterId"].as_str() else {
                continue;
            };
            let Ok(current) = chapter(db, book_id, cid).await else {
                continue;
            };
            (
                artifact["content"].as_str().unwrap_or("").to_owned(),
                current["textContent"].as_str().unwrap_or("").to_owned(),
                "unknown",
                current["localVersion"].clone(),
            )
        } else {
            continue;
        };
        if before.trim().is_empty() || after.trim().is_empty() || before == after {
            continue;
        }
        let fingerprint =
            crate::agent_creation::version(&json!([artifact["id"], revision, before, after]));
        let id = format!("edit:{fingerprint}");
        let record = json!({"id":id,"bookId":book_id,"artifactId":artifact["id"],"chapterId":artifact["chapterId"],
            "artifactRevision":artifact["revision"],"afterRevision":revision,"origin":origin,"before":before,"after":after,
            "rulesetId":artifact["rulesetId"],"writingBasisFingerprint":artifact["writingBasis"]["fingerprint"],"createdAt":now()});
        if get(db, NS, &id).await?.is_none() {
            put(db, NS, &id, &record).await?;
        }
        collected += 1;
    }
    Ok(
        json!({"samples":collected,"proposed":[],"note":"保留改前改后及来源，不从删词或段长自动推断写作规则"}),
    )
}

pub async fn evidence(db: &mut SqliteConnection, book_id: &str) -> Result<Vec<Value>, String> {
    learn(db, book_id).await?;
    let mut result = Vec::new();
    for record in list(db, NS, "edit:").await? {
        if record["bookId"] != book_id || record["origin"] != "author_edit" {
            continue;
        }
        let Some(aid) = record["artifactId"].as_str() else {
            continue;
        };
        let Some(a) = get(db, NS, &format!("artifact:{aid}")).await? else {
            continue;
        };
        if a["deletedAt"].is_number()
            || a["revision"] != record["artifactRevision"]
            || a["content"] != record["after"]
        {
            continue;
        }
        result.push(record);
    }
    result.sort_by_key(|e| std::cmp::Reverse(e["createdAt"].as_i64().unwrap_or(0)));
    Ok(result)
}

pub async fn propose(
    db: &mut SqliteConnection,
    book_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let available = evidence(db, book_id).await?;
    let ids = args["evidence_ids"].as_array().ok_or("缺少修改依据")?;
    if ids.is_empty() || ids.len() > 3 {
        return Err("请选择一至三份真实修改依据".into());
    }
    let mut selected = Vec::new();
    for id in ids {
        let item = available
            .iter()
            .find(|e| e["id"] == *id)
            .ok_or("修改依据已过期或不属于本书")?;
        if !selected.iter().any(|e: &Value| e["id"] == *id) {
            selected.push(item.clone());
        }
    }
    let observation = str_field(args, "observation")?;
    let suggestion = str_field(args, "suggestion")?;
    let id = format!(
        "feedback-{}",
        crate::agent_creation::version(&json!([book_id, suggestion]))
    );
    let key = format!("memory:{id}");
    if let Some(existing) = get(db, NS, &key).await? {
        return Ok(json!({"id":id,"status":existing["status"],"cached":true}));
    }
    let quote = selected
        .iter()
        .map(|e| {
            format!(
                "改前：{}\n改后：{}",
                e["before"].as_str().unwrap_or(""),
                e["after"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let record = json!({"id":id,"bookId":book_id,"content":observation,"category":"写作偏好","kind":"candidate",
        "scope":"book","status":"pending","source":"结合完整改稿对照提出，尚待作者确认","quote":quote,"period":"本书",
        "suggestedRule":{"category":"文风","text":suggestion},"evidence":{"editIds":ids},"revision":0,"createdAt":now(),"updatedAt":now()});
    put(db, NS, &key, &record).await?;
    Ok(json!({"id":id,"status":"pending","message":"偏好建议已保存，作者确认前不作为写作要求"}))
}
