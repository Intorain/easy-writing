//! 技能运行许可与卸载的领域边界。许可绑定作者点选和文件指纹；卸载保留可恢复副本。
use crate::agent_store::{err, get, id, list, now, put, session, str_field, NS, OP_NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub fn gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// None 表示放行；Some 是发起确认或作者拒绝的工具结果。
pub async fn consent(
    db: &mut SqliteConnection,
    s: &Value,
    skill: &mut Value,
    action: &str,
) -> Result<Option<Value>, String> {
    let sid = str_field(s, "id")?;
    let skill_id = str_field(skill, "id")?.to_owned();
    if skill["deletedAt"].is_number() {
        return Err("技能已卸载".into());
    }
    let preview = crate::agent_skill_files::read_draft(skill)?;
    let hash = if action == "run" {
        preview["scriptHash"].clone()
    } else {
        preview["revision"].clone()
    };
    if action == "run"
        && (!crate::agent_skill_runner::needs_approval(skill) || skill["approvedHash"] == hash)
    {
        return Ok(None);
    }
    let key = format!("skill-consent:{sid}:{action}:{skill_id}");
    if let Some(old) = get(db, OP_NS, &key)
        .await?
        .filter(|v| v["hash"] == hash && v["decision"].is_null())
    {
        if let Some((picked, reply)) =
            crate::agent_prompt::confirmation_answer(s, old["token"].as_str().unwrap_or(""))
        {
            let approved = picked.as_deref() == Some("0");
            let mut decided = old.clone();
            decided["decision"] = json!(if approved { "approved" } else { "declined" });
            put(db, OP_NS, &key, &decided).await?;
            if !approved {
                return Ok(Some(
                    json!({"status":"declined","message":"作者没有确认，本次未执行。","authorReply":reply}),
                ));
            }
            if action == "run" {
                skill["approvedHash"] = hash;
                skill["approvedAt"] = json!(now());
                put(db, NS, &format!("skill:{skill_id}"), skill).await?;
            }
            return Ok(None);
        }
        return Ok(Some(old["result"].clone()));
    }
    let token = id();
    let name = skill["name"].as_str().unwrap_or(&skill_id);
    let question = if action == "run" {
        format!("允许运行「{name}」的当前版本脚本吗？可以先查看完整文件。脚本以你的用户权限运行，能够访问本机文件和网络；文件或依赖清单改变后会重新确认。")
    } else {
        format!("卸载「{name}」吗？技能文件和专用依赖环境将移出可运行目录，并保留恢复副本。已有对话和成果会保留。")
    };
    let choices = if action == "run" {
        vec!["允许运行当前版本", "取消"]
    } else {
        vec!["卸载技能", "取消"]
    };
    let result = json!({"status":"confirmation_required","confirmation":{"token":token,"question":question,"choices":choices}});
    let record = json!({"token":token,"skillId":skill_id,"hash":hash,"action":action,"result":result,"issuedAt":now()});
    put(db, OP_NS, &key, &record).await?;
    // 只由宿主在 waiting 事件中装入预览入口，模型不能编造授权卡元数据。
    put(
        db,
        OP_NS,
        &format!("skill-consent-token:{sid}:{token}"),
        &record,
    )
    .await?;
    Ok(Some(result))
}

fn paths(root: &Path, user_dir: &Path, skill_id: &str, token: &str) -> [(PathBuf, PathBuf); 2] {
    [
        (
            user_dir.join(skill_id),
            user_dir.join(".uninstalled").join(token).join(skill_id),
        ),
        (
            root.join("agent/skill-env").join(skill_id),
            root.join("agent/skill-env/.uninstalled")
                .join(token)
                .join(skill_id),
        ),
    ]
}

/// 崩溃后依据数据库的卸载回执决定完成卸载还是回滚文件。路径只由校验过的编号派生。
pub async fn recover(
    db: &mut SqliteConnection,
    root: &Path,
    user_dir: &Path,
) -> Result<(), String> {
    let journals = root.join("agent/skill-uninstall");
    if !journals.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&journals).map_err(err)? {
        let entry = entry.map_err(err)?;
        if !entry.file_type().map_err(err)?.is_file() {
            return Err("技能卸载日志不是普通文件".into());
        }
        let record: Value =
            serde_json::from_slice(&fs::read(entry.path()).map_err(err)?).map_err(err)?;
        let skill_id = str_field(&record, "id")?;
        let token = str_field(&record, "token")?;
        if !crate::agent_skill_files::valid_id(skill_id)
            || !crate::agent_skill_files::valid_id(token)
        {
            return Err("技能卸载日志无效".into());
        }
        let installed = get(db, NS, &format!("skill:{skill_id}")).await?;
        let committed = installed
            .as_ref()
            .is_some_and(|s| s["uninstallToken"] == token);
        if !committed {
            for (source, archived) in paths(root, user_dir, skill_id, token) {
                if archived.exists() {
                    if source.exists() {
                        return Err("技能卸载回滚遇到同名目录，请先处理恢复副本".into());
                    }
                    fs::rename(archived, source).map_err(err)?;
                }
            }
        }
        fs::remove_file(entry.path()).map_err(err)?;
    }
    Ok(())
}

/// 调用方持有技能文件锁。文件离开运行目录后提交记录；失败和中断都能按日志回滚。
pub async fn uninstall(
    root: &Path,
    user_dir: &Path,
    skill_id: &str,
    current_session: Option<&str>,
) -> Result<Value, String> {
    if !crate::agent_skill_files::valid_id(skill_id) {
        return Err("技能编号无效".into());
    }
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    recover(&mut db, root, user_dir).await?;
    let mut record = get(&mut db, NS, &format!("skill:{skill_id}"))
        .await?
        .ok_or("技能不存在")?;
    if record["deletedAt"].is_number() {
        return Ok(record);
    }
    if record["source"] == "builtin" {
        return Err("内置技能不能卸载，可以停用或恢复默认".into());
    }
    if list(&mut db, NS, "session:")
        .await?
        .iter()
        .any(|s| s["status"] == "running" && s["id"].as_str() != current_session)
    {
        return Err("还有其他 Agent 任务在运行，请停止后再卸载技能".into());
    }
    let token = id();
    let moves = paths(root, user_dir, skill_id, &token);
    for (source, _) in &moves {
        if fs::symlink_metadata(source).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("技能或依赖目录不能是符号链接".into());
        }
    }
    if let Some(path) = record["filePath"].as_str() {
        if Path::new(path) != user_dir.join(skill_id).join("SKILL.md") {
            return Err("技能路径不属于作者技能目录，未移动文件".into());
        }
    }
    let journals = root.join("agent/skill-uninstall");
    fs::create_dir_all(&journals).map_err(err)?;
    let journal = journals.join(format!("{token}.json"));
    let mut log = fs::File::create(&journal).map_err(err)?;
    use std::io::Write;
    log.write_all(json!({"id":skill_id,"token":token}).to_string().as_bytes())
        .map_err(err)?;
    log.sync_all().map_err(err)?;
    let result = async {
        for (source, archived) in &moves {
            if source.exists() {
                fs::create_dir_all(archived.parent().ok_or("恢复路径无效")?).map_err(err)?;
                fs::rename(source, archived).map_err(err)?;
            }
        }
        record["deletedAt"] = json!(now());
        record["enabled"] = json!(false);
        record["status"] = json!("archived");
        record["uninstallToken"] = json!(token);
        record["recoveryPath"] = json!(moves[0].1);
        record["approvedHash"] = Value::Null;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        put(&mut tx, NS, &format!("skill:{skill_id}"), &record).await?;
        tx.commit().await.map_err(err)?;
        Ok::<_, String>(record)
    }
    .await;
    recover(&mut db, root, user_dir).await?;
    result
}

pub async fn request_uninstall(
    root: &Path,
    sid: &str,
    run: &str,
    skill_id: &str,
) -> Result<Value, String> {
    let _guard = gate().lock().await;
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let s = session(&mut db, sid).await?;
    if s["runId"] != run || s["status"] != "running" {
        return Err("任务已停止".into());
    }
    let mut skill = get(&mut db, NS, &format!("skill:{skill_id}"))
        .await?
        .ok_or("技能不存在")?;
    if skill["deletedAt"].is_number() {
        return Ok(json!({"uninstalled":true,"skillId":skill_id}));
    }
    if skill["source"] == "builtin" {
        return Err("内置技能不能卸载，可以在技能界面停用".into());
    }
    if let Some(result) = consent(&mut db, &s, &mut skill, "uninstall").await? {
        return Ok(result);
    }
    uninstall(
        root,
        &crate::ensure_prompt_dir()?.join("skills"),
        skill_id,
        Some(sid),
    )
    .await?;
    Ok(json!({"uninstalled":true,"skillId":skill_id}))
}
