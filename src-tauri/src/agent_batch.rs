//! 批量连载：作者授权连写多章。每章一次独立的会话运行（独立线程编号的检查点），
//! 每章开始前同步索引，按审阅策略停下；父会话只记录进度与每章成果链接。
//! 批量短篇（mode = short）：作者给一组选题，每个选题一次独立运行，产出结构卡与一篇短篇稿，不挂作品。
//!
//! 任务状态：queued → running → (waiting: question | review | issue | conflict) → running → completed；
//! 作者可 paused / cancelled；步骤失败为 failed。等待作者回答问题时，回答走普通对话，
//! 会话结束后由 `on_session_finished` 判断是否继续。
use crate::agent_store::{get, id, list, now, put, save_session, session, str_field, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::path::Path;

pub const REVIEW_EVERY: usize = 5;

fn job_key(job_id: &str) -> String {
    format!("job:{job_id}")
}

pub async fn load(db: &mut SqliteConnection, job_id: &str) -> Result<Value, String> {
    get(db, NS, &job_key(job_id))
        .await?
        .ok_or_else(|| "批量任务不存在".to_owned())
}

pub async fn list_all(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut jobs = list(db, NS, "job:").await?;
    jobs.retain(|j| !j["deletedAt"].is_number());
    jobs.sort_by_key(|j| -j["createdAt"].as_i64().unwrap_or(0));
    Ok(jobs)
}

/// A new conversational request must not finish or advance a waiting batch.
/// Keep its checkpoint and outputs so the batch can be resumed explicitly.
pub async fn suspend_for_message(db: &mut SqliteConnection, s: &mut Value) -> Result<(), String> {
    if let Some(job_id) = s["batchJobId"].as_str() {
        let mut job = load(db, job_id).await?;
        if ["queued", "running", "waiting"].contains(&job["status"].as_str().unwrap_or("")) {
            job["status"] = json!("paused");
            job["waitingReason"] = Value::Null;
            save(db, &mut job).await?;
        }
    }
    s["batchJobId"] = Value::Null;
    s["batchStep"] = Value::Null;
    s["batchWriteAuthorized"] = Value::Null;
    Ok(())
}

async fn save(db: &mut SqliteConnection, job: &mut Value) -> Result<(), String> {
    job["updatedAt"] = json!(now());
    job["revision"] = json!(job["revision"].as_u64().unwrap_or(0) + 1);
    put(db, NS, &job_key(str_field(job, "id")?), job).await
}

/// 建立批量任务：作者在界面上选章节、审阅策略与模式；直接写入模式由这一次界面操作授权写入目标。
pub async fn create(db: &mut SqliteConnection, args: &Value) -> Result<Value, String> {
    let sid = str_field(args, "sessionId")?;
    let mut s = session(db, sid).await?;
    if s["status"] == "running" {
        return Err("会话还有任务在执行，请等待完成或停止后再开始批量".into());
    }
    if list_all(db).await?.iter().any(|j| {
        j["sessionId"] == sid
            && ["queued", "running", "waiting", "paused", "failed"]
                .contains(&j["status"].as_str().unwrap_or(""))
    }) {
        return Err("本会话已有进行中的批量任务".into());
    }
    let policy = match args["reviewPolicy"].as_str().unwrap_or("onIssue") {
        "each" => "each",
        "every5" => "every5",
        _ => "onIssue",
    };
    if args["mode"] == "short" {
        return create_short(db, &mut s, args, policy).await;
    }
    let bid = str_field(args, "bookId")?;
    crate::agent_service::allowed_book(db, &s, bid, false).await?;
    let mode = match args["mode"].as_str().unwrap_or("candidate") {
        "write" => "write",
        _ => "candidate",
    };
    let catalog = crate::agent_store::catalog(db, bid).await?;
    let mut queue = Vec::new();
    for (index, item) in args["chapters"]
        .as_array()
        .ok_or("请选择要写的章节")?
        .iter()
        .enumerate()
    {
        let chapter_id = str_field(item, "chapterId")?;
        let entry = catalog
            .iter()
            .find(|c| c["id"].to_string() == chapter_id)
            .ok_or_else(|| format!("章节不存在：{chapter_id}"))?;
        queue.push(
            json!({"index":index,"chapterId":chapter_id,"title":entry["title"],"status":"pending"}),
        );
    }
    if queue.is_empty() || queue.len() > 50 {
        return Err("一次批量请选择 1 到 50 章".into());
    }
    let positions = queue
        .iter()
        .map(|q| {
            catalog
                .iter()
                .position(|c| c["id"].to_string() == q["chapterId"].as_str().unwrap_or(""))
                .unwrap()
        })
        .collect::<Vec<_>>();
    if positions.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return Err("批量连载必须选择按目录顺序排列的连续章节，不能重复或跳章".into());
    }
    let job_id = id();
    let job = json!({"id":job_id,"sessionId":sid,"bookId":bid,"bookTitle":crate::agent_store::book(db,bid).await?["title"],
        "mode":mode,"skillId":args["skillId"].as_str().or(s["selectedSkillId"].as_str()).unwrap_or(""),"reviewPolicy":policy,"modelCode":args["modelCode"].as_str().or(s["modelCode"].as_str()).unwrap_or(""),
        "queue":queue,"cursor":0,"status":"queued","waitingReason":Value::Null,"createdAt":now(),"revision":0,
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}});
    if mode == "write" {
        // 作者通过界面明确选择"直接写入"，这一次操作即为写入授权；每步运行仍会在工具里再次确认目标。
        s["batchWriteAuthorized"] = json!(bid);
    }
    s["batchJobId"] = json!(job_id);
    save_session(db, &mut s).await?;
    let mut job = job;
    save(db, &mut job).await?;
    Ok(job)
}

/// 批量短篇：选题一行一个，1 到 20 个；画像决定字数、段长与读者画像。
async fn create_short(
    db: &mut SqliteConnection,
    s: &mut Value,
    args: &Value,
    policy: &str,
) -> Result<Value, String> {
    let profile_id = args["profileId"]
        .as_str()
        .filter(|p| !p.is_empty())
        .unwrap_or(crate::agent_quality::SHORT_PROFILE);
    let profile = crate::agent_canon::platform_profile(db, profile_id).await?;
    let mut queue = Vec::new();
    for item in args["topics"].as_array().ok_or("请填写选题")? {
        let topic = item
            .as_str()
            .or_else(|| item["title"].as_str())
            .unwrap_or("")
            .trim();
        if topic.is_empty() {
            continue;
        }
        if topic.chars().count() > 120 {
            return Err("单个选题请控制在 120 字以内".into());
        }
        queue.push(json!({"index":queue.len(),"chapterId":"","title":topic,"topic":topic,"status":"pending"}));
    }
    if queue.is_empty() || queue.len() > 20 {
        return Err("一次批量短篇请填写 1 到 20 个选题".into());
    }
    let job_id = id();
    let mut job = json!({"id":job_id,"sessionId":s["id"],"bookId":"","bookTitle":format!("短篇 · {}", profile["name"].as_str().unwrap_or(profile_id)),
        "mode":"short","skillId":args["skillId"].as_str().or(s["selectedSkillId"].as_str()).unwrap_or(""),"profileId":profile_id,"reviewPolicy":policy,"modelCode":args["modelCode"].as_str().or(s["modelCode"].as_str()).unwrap_or(""),
        "queue":queue,"cursor":0,"status":"queued","waitingReason":Value::Null,"createdAt":now(),"revision":0,
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}});
    s["batchJobId"] = json!(job_id);
    save_session(db, s).await?;
    save(db, &mut job).await?;
    Ok(job)
}

/// 这一步用哪个技能：短篇按短篇方法，其余按章节写作。
pub fn step_skill(job: &Value) -> &str {
    job["skillId"].as_str().unwrap_or("")
}

/// pause / resume / cancel。resume 只从 waiting 或 paused 恢复；cancel 保留已完成成果。
pub async fn control(
    db: &mut SqliteConnection,
    job_id: &str,
    action: &str,
) -> Result<Value, String> {
    let mut job = load(db, job_id).await?;
    let status = job["status"].as_str().unwrap_or("").to_owned();
    match action {
        "pause" => {
            if !["queued", "running", "waiting"].contains(&status.as_str()) {
                return Err("任务不在可暂停的状态".into());
            }
            job["status"] = json!("paused");
            job["waitingReason"] = Value::Null;
        }
        "resume" => {
            if !["waiting", "paused", "queued", "failed"].contains(&status.as_str()) {
                return Err("任务不在可继续的状态".into());
            }
            let s = session(db, str_field(&job, "sessionId")?).await?;
            if s["status"] == "running" {
                return Err("当前步骤仍在停止或保存，请稍后继续".into());
            }
            if s["status"] == "choice" {
                return Err("请先在对话中回答当前问题".into());
            }
            let index = job["cursor"].as_u64().unwrap_or(0) as usize;
            if job["queue"][index]["status"] == "review" {
                let outcome = entry_outcome(db, &job, index).await?;
                if !delivery_ready(&job, &outcome) {
                    return Err("当前稿件尚未完成、来源已变化或仍有阻断问题，请先处理".into());
                }
                job["queue"][index]["status"] = json!("done");
                job["queue"][index]["reviewed"] = json!(true);
            } else if ["blocked", "paused", "failed", "running"]
                .contains(&job["queue"][index]["status"].as_str().unwrap_or(""))
            {
                job["queue"][index]["status"] = json!("pending");
            }
            job["status"] = json!(if all_done(&job) {
                "completed"
            } else {
                "running"
            });
            job["waitingReason"] = Value::Null;
            if all_done(&job) {
                detach(db, &job).await?;
            }
        }
        "cancel" => {
            if ["completed", "cancelled"].contains(&status.as_str()) {
                return Err("任务已经结束".into());
            }
            job["status"] = json!("cancelled");
            job["waitingReason"] = Value::Null;
            for entry in job["queue"].as_array_mut().into_iter().flatten() {
                if entry["status"] != "done" {
                    entry["status"] = json!("skipped");
                }
            }
            if let Ok(mut s) = session(db, str_field(&job, "sessionId")?).await {
                s["batchJobId"] = Value::Null;
                s["batchWriteAuthorized"] = Value::Null;
                save_session(db, &mut s).await?;
            }
        }
        _ => return Err("未知的批量操作".into()),
    }
    save(db, &mut job).await?;
    Ok(job)
}

/// 取下一章并标记 running；任务不在 running/queued 状态时返回 None。
pub async fn next_step(
    db: &mut SqliteConnection,
    job_id: &str,
) -> Result<Option<(Value, usize)>, String> {
    let mut job = load(db, job_id).await?;
    let status = job["status"].as_str().unwrap_or("");
    if !["queued", "running"].contains(&status) {
        return Ok(None);
    }
    if job["queue"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| e["status"] == "running")
    {
        return Ok(None);
    }
    let Some(index) = job["queue"]
        .as_array()
        .and_then(|q| q.iter().position(|e| e["status"] != "done"))
    else {
        job["status"] = json!("completed");
        detach(db, &job).await?;
        save(db, &mut job).await?;
        return Ok(None);
    };
    if job["queue"][index]["status"] != "pending" {
        return Ok(None);
    }
    job["status"] = json!("running");
    job["cursor"] = json!(index);
    job["queue"][index]["status"] = json!("running");
    job["queue"][index]["startedAt"] = json!(now());
    save(db, &mut job).await?;
    Ok(Some((job, index)))
}

/// 每一步交给会话的指令；仍然是普通 Agent 运行，工具权限与体检门不变。
pub fn step_instruction(job: &Value, index: usize) -> String {
    let entry = &job["queue"][index];
    let total = job["queue"].as_array().map_or(0, |q| q.len());
    if job["mode"] == "short" {
        return format!(
            "【批量短篇 第 {}/{} 篇】选题：{}\n按短篇写作的方法写一篇（结构卡 + 一稿 + 体检），不挂作品，短篇画像 profile_id={}。\n批量运行时作者不在场：选题里缺的题材、情绪核、身份反差由你补全，写进结构卡并说明；只有选题本身无法成立时才问作者。\n已有候选先检查并修订，不重复生成。完成后说明实际成果和待处理的问题。",
            index + 1,
            total,
            entry["topic"].as_str().or(entry["title"].as_str()).unwrap_or(""),
            job["profileId"].as_str().unwrap_or(crate::agent_quality::SHORT_PROFILE)
        );
    }
    // 写入授权来自作者在批量设置里的选择；平台仍按写入目标与体检结论核对
    let mode = if job["mode"] == "write" {
        "直接写入：作者已授权本批次写入正文。体检没有阻断时用 select_write_target 确定目标后 save_chapter 写入；有阻断先定向修订。"
    } else {
        "只出候选：保存章节候选即可，不写入正式正文。"
    };
    format!(
        "【批量连载 第 {}/{} 步：{}（chapter_id={}）】\n按本次选定的章节写作方法写这一章，并做体检（含读者评审）。\n模式：{}\n已有候选先读取、检查并修订；本批次前几章的候选是这一章的前文，但还不是正式事实。完成后只报告实际成果与未解决的问题。需要作者决定的事用 ask_author 停下来问，不要跳过。",
        index + 1,
        total,
        entry["title"].as_str().unwrap_or(""),
        entry["chapterId"].as_str().unwrap_or(""),
        format!("{}\n当前步骤已有候选：{}", mode, entry["artifactId"].as_str().unwrap_or("无"))
    )
}

fn all_done(job: &Value) -> bool {
    job["queue"]
        .as_array()
        .is_some_and(|q| !q.is_empty() && q.iter().all(|e| e["status"] == "done"))
}

async fn detach(db: &mut SqliteConnection, job: &Value) -> Result<(), String> {
    let mut s = session(db, str_field(job, "sessionId")?).await?;
    if s["batchJobId"] == job["id"] {
        s["batchJobId"] = Value::Null;
        s["batchStep"] = Value::Null;
        s["batchWriteAuthorized"] = Value::Null;
        save_session(db, &mut s).await?;
    }
    Ok(())
}

fn delivery_ready(job: &Value, outcome: &Value) -> bool {
    outcome["artifactId"].is_string()
        && outcome["partial"] != true
        && outcome["reportCurrent"] == true
        && (outcome["blocked"] != true || outcome["waived"] == true)
        && outcome["error"].as_str().map_or(true, str::is_empty)
        && (job["mode"] != "write" || outcome["applied"] == true)
}

async fn artifact_outcome(
    db: &mut SqliteConnection,
    artifact: Option<Value>,
) -> Result<Value, String> {
    let Some(mut a) = artifact else {
        return Ok(json!({"artifactId":null,"error":"本步骤没有生成正文成果"}));
    };
    let mut error = verify_candidate_sources(db, &a).await.err();
    let mut applied = a["applied"] == true;
    if applied && a["kind"] == "chapter" {
        let body =
            crate::agent_store::chapter(db, str_field(&a, "bookId")?, str_field(&a, "chapterId")?)
                .await?;
        applied =
            body["localVersion"] == a["appliedVersion"] && body["textContent"] == a["content"];
        if !applied {
            error = Some("正式正文已变化，需重新核对当前步骤".into());
        }
    }
    let report = if a["partial"] == true {
        Value::Null
    } else {
        let report = crate::agent_submission::refresh_report(db, &mut a).await?;
        put(db, NS, &format!("artifact:{}", str_field(&a, "id")?), &a).await?;
        report
    };
    let suspects = report["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["level"] == "suspect")
        .count();
    Ok(
        json!({"artifactId":a["id"],"applied":applied,"partial":a["partial"],"blocked":report["blocked"],
        "reportCurrent":report.is_object() && crate::agent_submission::report_current(&a,&report),
        "waived":crate::agent_submission::waiver_current(&a,&report),"suspects":suspects,"error":error}),
    )
}

async fn entry_outcome(
    db: &mut SqliteConnection,
    job: &Value,
    index: usize,
) -> Result<Value, String> {
    let a = match job["queue"][index]["artifactId"].as_str() {
        Some(aid) => get(db, NS, &format!("artifact:{aid}")).await?,
        None => None,
    };
    artifact_outcome(db, a).await
}

/// 按批次和步骤认领成果，恢复时不依赖会变化的 runId；不误用其他章节或其他批次的候选。
pub async fn outcome_from_session(
    db: &mut SqliteConnection,
    s: &Value,
    run_id: &str,
) -> Result<Value, String> {
    let job = load(db, str_field(s, "batchJobId")?).await?;
    let index = job["cursor"].as_u64().unwrap_or(0) as usize;
    let mut candidates = list(db, NS, "artifact:")
        .await?
        .into_iter()
        .filter(|a| {
            a["sessionId"] == s["id"]
                && crate::agent_quality::is_prose(a)
                && (a["batchJobId"] == job["id"] && a["batchIndex"] == index
                    || a["runId"] == run_id
                    || a["id"] == job["queue"][index]["artifactId"])
                && (job["mode"] == "short" && a["kind"] == "short"
                    || a["bookId"] == job["bookId"]
                        && a["chapterId"] == job["queue"][index]["chapterId"])
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|a| a["createdAt"].as_i64().unwrap_or(0));
    let mut out = artifact_outcome(db, candidates.pop()).await?;
    out["sessionStatus"] = s["status"].clone();
    out["runId"] = json!(run_id);
    out["usage"] = s["usage"].clone();
    if !s["error"].is_null() {
        out["error"] = s["error"].clone();
    }
    Ok(out)
}

/// 成果满足交付条件才完成步骤；审阅策略只决定停顿时机，不能绕过缺稿、半稿和保存失败。
pub async fn record_step(
    db: &mut SqliteConnection,
    job_id: &str,
    index: usize,
    outcome: &Value,
) -> Result<Value, String> {
    let mut job = load(db, job_id).await?;
    if index >= job["queue"].as_array().map_or(0, Vec::len) {
        return Err("批量步骤不存在".into());
    }
    if job["status"] == "cancelled" {
        return Ok(job);
    }
    let session_status = outcome["sessionStatus"].as_str().unwrap_or("");
    let completed = session_status == "completed";
    let valid = completed && delivery_ready(&job, outcome);
    let review = valid
        && match job["reviewPolicy"].as_str().unwrap_or("onIssue") {
            "each" => true,
            "every5" => {
                (index + 1) % REVIEW_EVERY == 0
                    || index + 1 == job["queue"].as_array().unwrap().len()
            }
            _ => outcome["suspects"].as_u64().unwrap_or(0) > 0,
        };
    let step_status = match session_status {
        "completed" if !valid => "blocked",
        "completed" if review => "review",
        "completed" => "done",
        "choice" => "running",
        "paused" => "paused",
        _ => "failed",
    };
    let entry = &mut job["queue"][index];
    for field in [
        "runId",
        "artifactId",
        "blocked",
        "suspects",
        "applied",
        "partial",
        "reportCurrent",
        "error",
    ] {
        if field == "artifactId" && outcome[field].is_null() && entry[field].is_string() {
            continue;
        }
        entry[field] = outcome[field].clone();
    }
    entry["status"] = json!(step_status);
    entry["finishedAt"] = json!(now());
    entry["usage"] = outcome["usage"].clone();
    if completed && !valid && entry["error"].is_null() {
        entry["error"] = json!("正文未完成、体检未通过或尚未成功写入，继续时处理本步骤");
    }
    // 从步骤汇总，恢复重放同一步不会重复累加。
    let mut usage = json!({});
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        usage[key] = json!(job["queue"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["usage"][key].as_u64().unwrap_or(0))
            .sum::<u64>());
    }
    job["usage"] = usage;
    if job["status"] != "paused" {
        let (status, reason) = match step_status {
            "running" => ("waiting", Some("question")),
            "blocked" => ("waiting", Some("issue")),
            "review" => ("waiting", Some("review")),
            "paused" => ("paused", None),
            "failed" => ("failed", Some("error")),
            _ if all_done(&job) => ("completed", None),
            _ => ("running", None),
        };
        job["status"] = json!(status);
        job["waitingReason"] = json!(reason);
    }
    if all_done(&job) {
        detach(db, &job).await?;
    }
    save(db, &mut job).await?;
    Ok(job)
}

/// 会话（普通对话路径）结束后：如果它是批量任务里等作者回答的那一步，把结果记进任务并继续。
pub fn on_session_finished(app: &crate::DesktopAppHandle, root: &Path, session_value: &Value) {
    let Some(job_id) = session_value["batchJobId"].as_str().map(str::to_owned) else {
        return;
    };
    let run_id = session_value["runId"].as_str().unwrap_or("").to_owned();
    let continued = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut job = load(&mut db, &job_id).await?;
        if !["waiting", "paused", "failed"].contains(&job["status"].as_str().unwrap_or("")) {
            return Ok::<bool, String>(false);
        }
        let index = job["cursor"].as_u64().unwrap_or(0) as usize;
        let thread = format!("{}:batch:{job_id}:{index}", str_field(session_value, "id")?);
        if session_value["activeThreadId"] != thread {
            return Ok(false);
        }
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        job["status"] = json!("running");
        save(&mut tx, &mut job).await?;
        let outcome = outcome_from_session(&mut tx, session_value, &run_id).await?;
        let job = record_step(&mut tx, &job_id, index, &outcome).await?;
        tx.commit().await.map_err(crate::agent_store::err)?;
        crate::agent_events::emit(app, "agent-batch", &job);
        Ok(job["status"] == "running")
    });
    let continued = match continued {
        Ok(value) => value,
        Err(error) => {
            let failed = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(root).await?;
                let mut job = load(&mut db, &job_id).await?;
                job["status"] = json!("failed");
                job["waitingReason"] = json!("error");
                let index = job["cursor"].as_u64().unwrap_or(0) as usize;
                job["queue"][index]["status"] = json!("failed");
                job["queue"][index]["error"] = json!(error);
                save(&mut db, &mut job).await?;
                Ok::<Value, String>(job)
            });
            if let Ok(job) = failed {
                crate::agent_events::emit(app, "agent-batch", job);
            }
            crate::agent_events::emit(app,
                "agent-batch",
                json!({"jobId":job_id,"event":"runner_finished","error":error}),
            );
            false
        }
    };
    if continued {
        crate::agent_host::schedule_batch(app.clone(), job_id);
    }
}

/// 只向当前批次注入前置候选，不写入正式正文或作品状态索引。
pub async fn prior_candidates(
    db: &mut SqliteConnection,
    s: Option<&Value>,
    book_id: &str,
    chapter_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let Some(s) = s else {
        return Ok(Vec::new());
    };
    let Some(jid) = s["batchJobId"].as_str() else {
        return Ok(Vec::new());
    };
    let job = load(db, jid).await?;
    if job["bookId"] != book_id || job["mode"] == "short" {
        return Ok(Vec::new());
    }
    let queue = job["queue"].as_array().ok_or("批量目录损坏")?;
    let Some(index) = chapter_id.and_then(|cid| queue.iter().position(|e| e["chapterId"] == cid))
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in &queue[..index] {
        if entry["status"] != "done" {
            return Err("前置章节尚未完成，不能生成下一章".into());
        }
        let aid = str_field(entry, "artifactId")?;
        let a = get(db, NS, &format!("artifact:{aid}"))
            .await?
            .ok_or("前章候选已不存在，请重新处理该步骤")?;
        if a["sessionId"] != s["id"]
            || a["bookId"] != book_id
            || a["chapterId"] != entry["chapterId"]
            || a["partial"] == true
        {
            return Err("前章候选不完整或来源不匹配".into());
        }
        verify_candidate_sources(db, &a).await?;
        let body = crate::agent_store::chapter(db, book_id, str_field(entry, "chapterId")?).await?;
        if a["applied"] == true {
            if body["localVersion"] != a["appliedVersion"] || body["textContent"] != a["content"] {
                return Err("前章正式正文已变化，请先核对批量任务".into());
            }
            continue;
        }
        if body["localVersion"] != a["expectedVersion"] {
            return Err("前章候选的原文版本已变化，请重新核对".into());
        }
        if !delivery_ready(
            &json!({"mode":"candidate"}),
            &artifact_outcome(db, Some(a.clone())).await?,
        ) {
            return Err("前章候选尚未通过当前质量检查，请先修订".into());
        }
        out.push(a);
    }
    Ok(out)
}

pub fn candidate_sources(candidates: &[Value]) -> Value {
    json!(candidates.iter().map(|a| json!({"artifactId":a["id"],"revision":a["revision"],"bookId":a["bookId"],"chapterId":a["chapterId"],
        "fingerprint":crate::agent_quality::fingerprint(a["content"].as_str().unwrap_or(""))})).collect::<Vec<_>>())
}

/// 候选依赖绑定来源版本，作者改了前章之后，后章不得沿用旧候选静默写入。
pub async fn verify_candidate_sources(db: &mut SqliteConnection, a: &Value) -> Result<(), String> {
    for source in a["candidateSources"].as_array().into_iter().flatten() {
        let prior = get(
            db,
            NS,
            &format!("artifact:{}", str_field(source, "artifactId")?),
        )
        .await?
        .ok_or("依赖的前章候选已不存在")?;
        let same_revision = prior["revision"] == source["revision"]
            || prior["applied"] == true && prior["appliedSourceRevision"] == source["revision"];
        if !same_revision
            || prior["partial"] == true
            || prior["bookId"] != source["bookId"]
            || prior["chapterId"] != source["chapterId"]
            || crate::agent_quality::fingerprint(prior["content"].as_str().unwrap_or(""))
                != source["fingerprint"]
        {
            return Err("前章候选已修改，请基于当前前文重新核对或生成本章".into());
        }
        let body = crate::agent_store::chapter(
            db,
            str_field(source, "bookId")?,
            str_field(source, "chapterId")?,
        )
        .await?;
        let valid = if prior["applied"] == true {
            body["localVersion"] == prior["appliedVersion"]
                && body["textContent"] == prior["content"]
        } else {
            body["localVersion"] == prior["expectedVersion"]
        };
        if !valid {
            return Err("前章正文已变化，请基于当前前文重新核对或生成本章".into());
        }
    }
    Ok(())
}

/// 同步索引后逐一核对本章真正依赖的正式前文；无模型、暂停、重试失败都不能伪装为就绪。
pub async fn ensure_index_ready(
    db: &mut SqliteConnection,
    job: &Value,
    index: usize,
) -> Result<(), String> {
    if job["mode"] == "short" {
        return Ok(());
    }
    let bid = str_field(job, "bookId")?;
    let cid = str_field(&job["queue"][index], "chapterId")?;
    let entries = crate::agent_store::catalog(db, bid).await?;
    for entry in entries.iter().take_while(|e| e["id"].to_string() != cid) {
        let previous = entry["id"].to_string();
        let body = crate::agent_store::chapter(db, bid, &previous).await?;
        if body["textContent"].as_str().unwrap_or("").trim().is_empty() {
            continue;
        }
        let expected = crate::agent_indexer::chapter_fingerprint(db, bid, &previous).await?;
        let card = crate::agent_canon::get_card(db, bid, &previous).await?;
        if card.as_ref().map_or(true, |c| {
            c["status"] != "active" || c["sourceRevision"].as_str() != expected.as_deref()
        }) {
            return Err(format!(
                "前章「{}」的状态索引尚未更新，请完成索引后继续",
                entry["title"].as_str().unwrap_or(&previous)
            ));
        }
    }
    Ok(())
}
