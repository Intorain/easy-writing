//! 拆书任务：作者选定参考书的一段章节（开篇、前 N 章、全书），逐章提取章节卡。
//!
//! 每本参考书一条任务记录（`breakdown-job:{项目}`），只存区间、状态与用量；进度不另存，
//! 每次按章节卡与 outbox 现算，任务记录和实际数据不会对不上。提取本身走索引器的参考书通道：
//! 事件持久在 outbox，应用退出后下次启动接着拆；暂停用作品的索引暂停开关，取消丢弃未处理事件。
//!
//! 状态：running → completed；作者可 paused / cancelled；模型整轮失败或没有可用模型时 failed（可继续）。
use crate::agent_breakdown as breakdown;
use crate::agent_indexer as indexer;
use crate::agent_store::{get, now, put, str_field, session, save_session, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::BTreeSet;

/// 预估口径（粗估，界面与确认卡都标“约”）：正文按每字 1 token 计，加上每章固定开销
/// （提取规则、人物表、线索、结构化输出的格式说明），输出按每章固定值计。
pub const TOKENS_PER_CHAR: f64 = 1.0;
pub const INPUT_OVERHEAD: u64 = 3_000;
pub const OUTPUT_PER_CHAPTER: u64 = 1_200;
/// 单章耗时的大致范围（秒），按一章接一章顺序提取。
pub const SECONDS_PER_CHAPTER: (u64, u64) = (20, 45);
/// 正文超过这个字数的部分不交给模型（与索引器一致）。
const TEXT_LIMIT: usize = 40_000;

pub fn job_key(pid: &str) -> String {
    format!("breakdown-job:{pid}")
}

/// 区间按目录位置计（第 1 章起）。`to` 为 0 或超过总章数时取到最后一章。
fn clamp(from: i64, to: i64, total: usize) -> Result<(usize, usize), String> {
    if total == 0 {
        return Err("参考书没有章节".into());
    }
    let from = from.max(1) as usize;
    let to = if to <= 0 { total } else { (to as usize).min(total) };
    if from > total {
        return Err(format!("参考书只有 {total} 章"));
    }
    if from > to {
        return Err("起止章节无效".into());
    }
    Ok((from, to))
}

/// 区间内的章节：(编号, 标题, 字数)。
async fn range_chapters(db: &mut SqliteConnection, pid: &str, from: usize, to: usize) -> Result<Vec<(String, String, usize)>, String> {
    let catalog = breakdown::catalog(db, pid).await?;
    Ok(catalog[from - 1..to]
        .iter()
        .map(|c| (c["id"].to_string(), c["title"].as_str().unwrap_or("").to_owned(), c["wordCount"].as_u64().unwrap_or(0) as usize))
        .collect())
}

fn estimate_of(chars: &[usize]) -> Value {
    let chapters = chars.len() as u64;
    let input: u64 = chars.iter().map(|c| ((*c).min(TEXT_LIMIT) as f64 * TOKENS_PER_CHAR) as u64 + INPUT_OVERHEAD).sum();
    let output = chapters * OUTPUT_PER_CHAPTER;
    json!({"chapters":chapters,"chars":chars.iter().sum::<usize>(),"inputTokens":input,"outputTokens":output,"totalTokens":input + output,
        "minutes":[(chapters * SECONDS_PER_CHAPTER.0).div_ceil(60),(chapters * SECONDS_PER_CHAPTER.1).div_ceil(60)]})
}

/// 拆某个区间要花多少：只算还没有章节卡的章节。
pub async fn estimate(db: &mut SqliteConnection, pid: &str, from: i64, to: i64) -> Result<Value, String> {
    let p = breakdown::project(db, pid).await?;
    let total = p["chapters"].as_array().map_or(0, |a| a.len());
    let (from, to) = clamp(from, to, total)?;
    let chapters = range_chapters(db, pid, from, to).await?;
    let indexed = breakdown::active_card_chapters(db, &breakdown::book_id(pid)).await?;
    let missing: Vec<usize> = chapters.iter().filter(|(id, _, _)| !indexed.contains(id)).map(|(_, _, w)| *w).collect();
    Ok(json!({"projectId":pid,"title":p["title"],"from":from,"to":to,"totalChapters":total,"rangeChapters":chapters.len(),
        "alreadyIndexed":chapters.len() - missing.len(),"estimate":estimate_of(&missing)}))
}

async fn save(db: &mut SqliteConnection, job: &mut Value) -> Result<(), String> {
    job["updatedAt"] = json!(now());
    job["revision"] = json!(job["revision"].as_u64().unwrap_or(0) + 1);
    put(db, NS, &job_key(str_field(job, "projectId")?), job).await?;
    sync_conversation_card(db, job).await
}

/// 新建或替换这本书的拆书任务。之前排队、不在新区间里的章节一并撤掉，已写成的章节卡保留。
/// `auto_continue`：拆完后由宿主在发起的会话里接着完成作者的要求（Agent 发起时默认开，作者可在进度卡上关掉）。
pub async fn start(db: &mut SqliteConnection, pid: &str, from: i64, to: i64, session_id: Option<&str>, auto_continue: bool) -> Result<Value, String> {
    let p = breakdown::project(db, pid).await?;
    let total = p["chapters"].as_array().map_or(0, |a| a.len());
    let (from, to) = clamp(from, to, total)?;
    let chapters = range_chapters(db, pid, from, to).await?;
    let ids: Vec<String> = chapters.iter().map(|(id, _, _)| id.clone()).collect();
    let bid = breakdown::book_id(pid);
    indexer::drop_pending(db, &bid).await?;
    indexer::reset_failures(db, &bid, &ids).await?;
    indexer::set_index_paused(db, &bid, false).await?;
    let indexed = breakdown::active_card_chapters(db, &bid).await?;
    let missing: Vec<usize> = chapters.iter().filter(|(id, _, _)| !indexed.contains(id)).map(|(_, _, w)| *w).collect();
    let queued = indexer::enqueue_chapters(db, &bid, &ids).await?;
    let old = get(db, NS, &job_key(pid)).await?;
    if let Some(mut previous)=old.clone() {
        if ["running","paused","failed"].iter().any(|s| previous["status"]==*s) {
            previous["status"]=json!("cancelled");
            sync_conversation_card(db,&previous).await?;
        }
    }
    let stamp = now();
    let mut job = json!({"id":job_key(pid),"projectId":pid,"bookId":bid,"title":p["title"],"from":from,"to":to,
        "chapterIds":ids,"total":ids.len(),"firstTitle":chapters.first().map(|c| c.1.clone()),"lastTitle":chapters.last().map(|c| c.1.clone()),
        "status":if queued > 0 { "running" } else { "completed" },"estimate":estimate_of(&missing),
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0},"sessionId":session_id,
        "autoContinue":auto_continue && session_id.is_some(),"continuedAt":null,"continueSkipped":null,
        "createdAt":stamp,"startedAt":stamp,"finishedAt":if queued > 0 { Value::Null } else { json!(stamp) },"error":null,
        "revision":old.as_ref().and_then(|o| o["revision"].as_u64()).unwrap_or(0)});
    save(db, &mut job).await?;
    present(db, job).await
}

/// 作者在进度卡上点暂停、继续、取消。
pub async fn control(db: &mut SqliteConnection, pid: &str, action: &str) -> Result<Value, String> {
    let mut job = get(db, NS, &job_key(pid)).await?.ok_or("这本参考书还没有拆书任务")?;
    let bid = breakdown::book_id(pid);
    match action {
        "pause" => {
            indexer::set_index_paused(db, &bid, true).await?;
            if job["status"] == "running" || job["status"] == "failed" {
                job["status"] = json!("paused");
            }
        }
        "resume" => {
            indexer::set_index_paused(db, &bid, false).await?;
            let ids: Vec<String> = job["chapterIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            indexer::reset_failures(db, &bid, &ids).await?;
            indexer::enqueue_chapters(db, &bid, &ids).await?;
            let pending = indexer::pending(db, Some(&bid)).await?;
            job["status"] = json!(if pending > 0 { "running" } else { "completed" });
            job["error"] = Value::Null;
            if pending == 0 {
                job["finishedAt"] = json!(now());
            }
        }
        "cancel" => {
            indexer::drop_pending(db, &bid).await?;
            indexer::set_index_paused(db, &bid, false).await?;
            job["status"] = json!("cancelled");
            job["finishedAt"] = json!(now());
        }
        // 作者不要拆完自动接着分析
        "auto_off" => job["autoContinue"] = json!(false),
        _ => return Err("未知的拆书任务操作".into()),
    }
    save(db, &mut job).await?;
    present(db, job).await
}

/// 任务记录加上现算的进度：已拆章数、放弃的章节、还在排队的事件、剩余时间。
async fn present(db: &mut SqliteConnection, mut job: Value) -> Result<Value, String> {
    let bid = str_field(&job, "bookId")?.to_owned();
    let ids: BTreeSet<String> = job["chapterIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let indexed = breakdown::active_card_chapters(db, &bid).await?;
    let given_up = indexer::given_up(db, &bid).await?;
    let done = ids.iter().filter(|id| indexed.contains(*id)).count();
    let failed: Vec<Value> = given_up
        .iter()
        .filter(|(id, _)| ids.contains(*id) && !indexed.contains(*id))
        .map(|(id, message)| json!({"chapterId":id,"message":message}))
        .collect();
    let pending = indexer::pending(db, Some(&bid)).await?;
    let left = ids.len().saturating_sub(done + failed.len()) as u64;
    job["done"] = json!(done);
    job["failed"] = json!(failed.len());
    job["failedChapters"] = json!(failed.into_iter().take(20).collect::<Vec<_>>());
    job["pending"] = json!(pending);
    job["remainingMinutes"] = json!([(left * SECONDS_PER_CHAPTER.0).div_ceil(60), (left * SECONDS_PER_CHAPTER.1).div_ceil(60)]);
    // 章节编号列表只给宿主用，界面与模型不需要
    if let Some(map) = job.as_object_mut() {
        map.remove("chapterIds");
    }
    Ok(job)
}

pub async fn view(db: &mut SqliteConnection, pid: &str) -> Result<Option<Value>, String> {
    match get(db, NS, &job_key(pid)).await? {
        Some(job) => Ok(Some(present(db, job).await?)),
        None => Ok(None),
    }
}

/// 全部拆书任务（参考书已删除的不列）。
pub async fn list_views(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for job in crate::agent_store::list(db, NS, "breakdown-job:").await? {
        let pid = job["projectId"].as_str().unwrap_or("").to_owned();
        if breakdown::project(db, &pid).await.is_err() {
            continue;
        }
        out.push(present(db, job).await?);
    }
    out.sort_by_key(|j| -j["updatedAt"].as_i64().unwrap_or(0));
    Ok(out)
}

/// 参考书通道每跑完一轮：记上这本书用掉的 token；没有排队的事件了就收尾。
/// 返回最新进度，以及这一轮是否刚好拆完（宿主据此决定要不要接着分析）。
pub async fn record_round(db: &mut SqliteConnection, bid: &str, usage: &Value) -> Result<Option<(Value, bool)>, String> {
    let Some(pid) = breakdown::project_of(bid) else {
        return Ok(None);
    };
    let Some(mut job) = get(db, NS, &job_key(pid)).await? else {
        return Ok(None);
    };
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        job["usage"][key] = json!(job["usage"][key].as_u64().unwrap_or(0) + usage[key].as_u64().unwrap_or(0));
    }
    let finished = job["status"] == "running" && indexer::pending(db, Some(bid)).await? == 0;
    if finished {
        job["status"] = json!("completed");
        job["finishedAt"] = json!(now());
    }
    save(db, &mut job).await?;
    Ok(Some((present(db, job).await?, finished)))
}

/// 拆完后自动接着分析的结果：continued 已在会话里发起；skipped 会话正忙或在等作者回答，留给作者在进度卡上点继续。
pub async fn mark_continue(db: &mut SqliteConnection, pid: &str, outcome: Result<(), String>) -> Result<Option<Value>, String> {
    let Some(mut job) = get(db, NS, &job_key(pid)).await? else {
        return Ok(None);
    };
    match outcome {
        Ok(()) => job["continuedAt"] = json!(now()),
        Err(reason) => job["continueSkipped"] = json!(reason),
    }
    save(db, &mut job).await?;
    Ok(Some(present(db, job).await?))
}

/// 通道跑不下去（没有可用模型、模型整轮失败、进程起不来）：进行中的任务标为失败，作者可点继续。
pub async fn fail_running(db: &mut SqliteConnection, message: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for mut job in crate::agent_store::list(db, NS, "breakdown-job:").await? {
        if job["status"] != "running" {
            continue;
        }
        let bid = job["bookId"].as_str().unwrap_or("").to_owned();
        if indexer::pending(db, Some(&bid)).await? == 0 {
            continue;
        }
        // 停在这里等作者点继续：供应商故障时后台反复重试，只会把每章的三次机会白白耗掉
        indexer::set_index_paused(db, &bid, true).await?;
        job["status"] = json!("failed");
        job["error"] = json!(message);
        save(db, &mut job).await?;
        out.push(present(db, job).await?);
    }
    Ok(out)
}

/// 卡片属于发起拆书的会话，保留在消息历史中；引用同一本书的其他会话不会收到卡片。
pub async fn sync_conversation_card(db: &mut SqliteConnection, job: &Value) -> Result<(), String> {
    let Some(sid)=job["sessionId"].as_str().filter(|s|!s.is_empty()) else { return Ok(()) };
    let Ok(mut s)=session(db,sid).await else { return Ok(()) };
    let card_id=format!("breakdown-result:{}:{}",job["projectId"].as_str().unwrap_or(""),job["createdAt"]);
    let payload=json!({"projectId":job["projectId"],"title":job["title"],"from":job["from"],"to":job["to"],"createdAt":job["createdAt"],"status":job["status"]});
    let messages=s["messages"].as_array_mut().ok_or("会话消息格式无效")?;
    if let Some(message)=messages.iter_mut().find(|m|m["id"]==card_id) {
        if message["breakdown"]==payload { return Ok(()) }
        message["breakdown"]=payload;
    } else {
        messages.push(json!({"id":card_id,"role":"assistant","kind":"result","text":"","breakdown":payload,"createdAt":job["createdAt"]}));
    }
    save_session(db,&mut s).await
}

pub async fn restore_conversation_cards(db: &mut SqliteConnection) -> Result<(),String> {
    for job in crate::agent_store::list(db,NS,"breakdown-job:").await? {
        if breakdown::project(db,job["projectId"].as_str().unwrap_or("")).await.is_ok() { sync_conversation_card(db,&job).await?; }
    }
    Ok(())
}
