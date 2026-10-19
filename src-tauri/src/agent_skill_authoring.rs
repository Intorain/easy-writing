//! 对话里的技能候选。模型只写会话成果，作者采用时才进入统一的技能文件保存入口。
use crate::{agent_skill_files as files, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::path::Path;

pub async fn read(db: &mut SqliteConnection, skill_id: &str) -> Result<Value, String> {
    let record = get(db, NS, &format!("skill:{skill_id}"))
        .await?
        .ok_or("技能不存在")?;
    if record["deletedAt"].is_number() {
        return Err("技能已删除".into());
    }
    files::read_draft(&record)
}

/// Model-facing reads separate the complete instructions from optional resources.
/// Local editor/authoring operations keep the full draft through `read` above.
pub async fn read_manifest(db:&mut SqliteConnection,skill_id:&str)->Result<Value,String> {
    let draft=read(db,skill_id).await?;
    let resources=draft["files"].as_array().into_iter().flatten().map(|file|json!({
        "path":file["path"],"editable":file["editable"],
        "characters":file["content"].as_str().map(|v|v.chars().count()),
        "textAvailable":file["content"].is_string()
    })).collect::<Vec<_>>();
    Ok(json!({"id":draft["id"],"name":draft["name"],"description":draft["description"],"kind":draft["kind"],
        "instructions":draft["instructions"],"revision":draft["revision"],"readOnly":draft["readOnly"],
        "replaces":draft["replaces"],"requirements":draft["requirements"],"files":resources}))
}

pub async fn read_resource(db:&mut SqliteConnection,skill_id:&str,path:&str,offset:usize,limit:usize)->Result<Value,String> {
    let draft=read(db,skill_id).await?;
    let Some(file)=draft["files"].as_array().into_iter().flatten().find(|file|file["path"]==path) else {
        return Ok(crate::agent_tool_boundary::failure("skill_resource_not_found",crate::agent_tool_boundary::FailureKind::Reference,
            "该技能没有这个资源，请使用 read_skill 返回的相对路径。",true,"read_skill",json!({"skillId":skill_id,"path":path})));
    };
    let Some(content)=file["content"].as_str() else {return Err("该资源不是可读取的文本文件".into());};
    let total=content.chars().count();let start=offset.min(total);let count=limit.clamp(1,20000);
    let text=content.chars().skip(start).take(count).collect::<String>();let end=start+text.chars().count();
    Ok(json!({"skillId":skill_id,"path":path,"revision":draft["revision"],"offset":start,"totalCharacters":total,
        "nextOffset":if end<total{Some(end)}else{None},"complete":end==total,"content":text}))
}

pub fn content(draft: &Value) -> String {
    let mut text = format!(
        "# {}\n\n{}\n\n{}",
        draft["name"].as_str().unwrap_or("技能候选"),
        draft["description"].as_str().unwrap_or(""),
        draft["instructions"].as_str().unwrap_or("")
    );
    if let Some(items) = draft["files"].as_array().filter(|v| !v.is_empty()) {
        text.push_str("\n\n## 文件\n");
        for file in items {
            text.push_str(&format!("\n- {}", file["path"].as_str().unwrap_or("")));
        }
    }
    text
}

pub async fn prepare(root: &Path, sid: &str, run_id: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let session = session(&mut tx, sid).await?;
    if session["runId"] != run_id || session["status"] != "running" {
        return Err("任务已停止或更新".into());
    }
    let op = format!("skill-draft:{sid}:{}", str_field(args, "operation_id")?);
    if let Some(done) = get(&mut tx, OP_NS, &op).await? {
        if done["request"] != *args {
            return Err("重复操作编号对应不同技能内容".into());
        }
        return get(
            &mut tx,
            NS,
            &format!("artifact:{}", str_field(&done, "artifactId")?),
        )
        .await?
        .ok_or("技能候选不存在".into());
    }
    let existing_id = args["artifact_id"].as_str().filter(|s| !s.is_empty());
    let existing = match existing_id {
        Some(aid) => Some(
            get(&mut tx, NS, &format!("artifact:{aid}"))
                .await?
                .ok_or("技能候选不存在")?,
        ),
        None => None,
    };
    if let Some(a) = &existing {
        if a["kind"] != "skill" || a["sessionId"] != sid || a["applied"] == true {
            return Err("该技能候选不可修改，请读取已安装技能后另建候选".into());
        }
        if args["expected_revision"].as_u64() != a["revision"].as_u64() {
            return Err("技能候选已被修改，请读取最新版本".into());
        }
    }
    let target = args["target_skill_id"].as_str().unwrap_or("");
    let mut draft = args["content"].clone();
    if !draft.is_object() {
        return Err("技能候选格式无效".into());
    }
    let mut preview = Value::Null;
    if let Some(a) = &existing {
        if (!target.is_empty() && a["skillDraft"]["id"] != target)
            || args["target_revision"]
                .as_str()
                .is_some_and(|r| !r.is_empty() && a["skillDraft"]["revision"] != r)
        {
            return Err("修改候选不能更换目标技能或文件版本，请另建候选".into());
        }
        draft["id"] = a["skillDraft"]["id"].clone();
        draft["revision"] = a["skillDraft"]["revision"].clone();
    } else {
        draft["id"] = json!(target);
        draft["revision"] = Value::Null;
        if !target.is_empty() {
            preview = read(&mut tx, target).await?;
            if preview["revision"] != args["target_revision"] {
                return Err("目标技能已变化，请先 read_skill 读取当前版本".into());
            }
            draft["revision"] = preview["revision"].clone();
        }
    }
    let parsed: files::Draft = serde_json::from_value(draft.clone()).map_err(err)?;
    files::validate(&parsed)?;
    let aid = existing_id.map(str::to_owned).unwrap_or_else(id);
    let previous_revision = existing.as_ref().and_then(|a| a["revision"].as_u64());
    let preserved = existing
        .as_ref()
        .map(|a| a["preservedFiles"].clone())
        .unwrap_or_else(|| {
            json!(preview["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| f["editable"] != true)
                .map(|f| f["path"].clone())
                .collect::<Vec<_>>())
        });
    let a = json!({"id":aid,"sessionId":sid,"runId":run_id,"kind":"skill","title":draft["name"],"bookTitle":"", "original":"",
        "content":content(&draft),"skillDraft":draft,"preservedFiles":preserved,
        "skillCopy":existing.as_ref().map(|a|a["skillCopy"].clone()).unwrap_or_else(||preview["readOnly"].clone()),
        "applied":false,"revision":previous_revision.map_or(0,|v|v+1),"updatedAt":now()});
    put(&mut tx, NS, &format!("artifact:{aid}"), &a).await?;
    put(
        &mut tx,
        OP_NS,
        &op,
        &json!({"request":args,"artifactId":aid}),
    )
    .await?;
    tx.commit().await.map_err(err)?;
    Ok(a)
}

/// Background jobs have their own durable authorization and must not impersonate
/// a running conversation. Stable artifact identity makes completion replay-safe.
pub async fn prepare_background(db: &mut SqliteConnection, sid: &str, job_id: &str, draft: &Value) -> Result<Value, String> {
    session(db, sid).await?;
    let aid = format!("distillation:{job_id}:skill");
    if let Some(existing) = get(db, NS, &format!("artifact:{aid}")).await? { return Ok(existing); }
    let parsed: files::Draft = serde_json::from_value(draft.clone()).map_err(err)?;
    files::validate(&parsed)?;
    let a = json!({"id":aid,"sessionId":sid,"runId":job_id,"kind":"skill","title":draft["name"],"bookTitle":"","original":"",
        "content":content(draft),"skillDraft":draft,"preservedFiles":[],"skillCopy":false,"applied":false,"revision":0,"updatedAt":now()});
    put(db, NS, &format!("artifact:{aid}"), &a).await?;
    Ok(a)
}

/// 面板自动保存允许未完成的内容，但不能改变宿主绑定的目标和来源版本；正式采用仍执行完整校验。
pub fn update(a: &mut Value, draft: &Value) -> Result<(), String> {
    let parsed: files::Draft = serde_json::from_value(draft.clone()).map_err(err)?;
    if draft["id"] != a["skillDraft"]["id"] || draft["revision"] != a["skillDraft"]["revision"] {
        return Err("不能更换技能候选的目标或来源版本".into());
    }
    files::validate_candidate(&parsed)?;
    a["skillDraft"] = draft.clone();
    a["title"] = draft["name"].clone();
    a["content"] = json!(content(draft));
    Ok(())
}

pub async fn adopt(
    db: &mut SqliteConnection,
    user_dir: &Path,
    aid: &str,
    revision: u64,
) -> Result<Value, String> {
    let key = format!("artifact:{aid}");
    let mut a = get(db, NS, &key).await?.ok_or("技能候选不存在")?;
    if a["kind"] != "skill" {
        return Err("该成果不是技能候选".into());
    }
    if a["applied"] == true && a["adoptedRevision"].as_u64() == Some(revision) {
        return Ok(a);
    }
    if a["applied"] == true || a["revision"].as_u64() != Some(revision) {
        return Err("技能候选已变化，请查看最新内容后再保存".into());
    }
    let draft: files::Draft = serde_json::from_value(a["skillDraft"].clone()).map_err(err)?;
    // Artifact keys are namespaced database identities, not portable directory
    // names. Keep existing UUID-based installation IDs; hash other identities
    // instead of replacing punctuation (which would create collisions).
    let legacy_id = format!("skill-{aid}");
    let target_id = if files::valid_id(&legacy_id) {
        legacy_id
    } else {
        format!("skill-{}", crate::agent_creation::version(&json!({"artifactId":aid})))
    };
    let saved = files::save_candidate(db, user_dir, &draft, &target_id).await?;
    a["installedSkillId"] = saved["id"].clone();
    a["applied"] = json!(true);
    a["adoptedRevision"] = json!(revision);
    a["revision"] = json!(revision + 1);
    a["adoptedAt"] = json!(now());
    put(db, NS, &key, &a).await?;
    Ok(a)
}
