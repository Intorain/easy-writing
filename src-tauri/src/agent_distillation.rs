//! Durable book distillation. Reuses the chapter outbox, worker pool, model gateway,
//! reference-book reader and skill-candidate store. Model observations are not canon.
//!
//! 产物是文风档案（`kind: style` 技能 + `references/examples.json`）：范例原文由程序按模型标出的段落取回，
//! 写法卡每条指向一段范例并经一次核验，叙事模式给规划模型；指纹由程序算。模型不写技能正文，也不出数字。
//! 节点：leaf（标句段）→ summary（归纳，容量不够时按输入大小分层）→ verify（一次核验全部写法卡）→ 程序编档。
use crate::{agent_breakdown as book, agent_indexer as queue, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const NODE_NS: &str = "ew-agent-distillation/records";
pub const TASK_KIND: &str = "distillation";
// Execution capacity only; neither value limits reading coverage or writing rules.
pub const WORKERS: usize = 4;
/// 每个分析节点本轮允许的模型请求数；Python 与 SDK 不另行重试。
pub const MAX_NODE_ATTEMPTS: usize = 3;
/// 这些错误码值得换个上下文再来：结果没写合格、模型没作答、引文对不上
const RETRYABLE_CODES: [&str; 4] = [
    "invalid_output",
    "transient_output",
    "empty_output",
    "distillation_evidence_invalid",
];
const PROTOCOL_VERSION: u64 = 2;
/// 一个阅读片段最多这么多字：片段太大，模型挑出来的段落就稀了。
const SLICE_MAX_CHARS: usize = 12_000;
fn job_key(jid: &str) -> String {
    format!("distillation-job:{jid}")
}
fn queue_book(jid: &str) -> String {
    format!("distillation:{jid}")
}
fn node_key(jid: &str, nid: &str) -> String {
    format!("{jid}:{nid}")
}
fn version(v: &Value) -> String {
    crate::agent_creation::version(v)
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn number(v: &Value) -> usize {
    v.as_u64().unwrap_or(0) as usize
}
pub async fn load(db: &mut SqliteConnection, jid: &str) -> Result<Value, String> {
    get(db, NS, &job_key(jid))
        .await?
        .ok_or("蒸馏任务不存在".into())
}
async fn nodes(db: &mut SqliteConnection, jid: &str) -> Result<Vec<Value>, String> {
    list(db, NODE_NS, &format!("{jid}:")).await
}
async fn save_job(db: &mut SqliteConnection, job: &mut Value) -> Result<(), String> {
    job["updatedAt"] = json!(now());
    job["revision"] = json!(job["revision"].as_u64().unwrap_or(0) + 1);
    put(db, NS, &job_key(str_field(job, "id")?), job).await
}
/// Reconstruct operational queue entries from durable nodes after restore/startup.
/// Only called when this host has no live distillation worker.
pub async fn recover(db: &mut SqliteConnection) -> Result<(), String> {
    queue::ensure_schema(db).await?;
    sqlx::query("UPDATE agent_outbox SET claimedBy=NULL,claimedAt=NULL WHERE kind=? AND processedAt IS NULL")
        .bind(TASK_KIND).execute(&mut *db).await.map_err(err)?;
    for job in list(db, NS, "distillation-job:").await? {
        if !["running", "paused", "failed"]
            .iter()
            .any(|s| job["status"] == *s)
        {
            continue;
        }
        let jid = str_field(&job, "id")?;
        queue::set_index_paused(db, &queue_book(jid), job["status"] != "running").await?;
        for n in nodes(db, jid)
            .await?
            .into_iter()
            .filter(|n| matches!(n["status"].as_str(), Some("pending" | "running")))
        {
            requeue(db, jid, str_field(&n, "id")?).await?;
        }
    }
    Ok(())
}

pub async fn scoped(db: &mut SqliteConnection, s: &Value, jid: &str) -> Result<Value, String> {
    let job = load(db, jid).await?;
    if job["sessionId"] != s["id"] {
        return Err("蒸馏任务不属于当前会话".into());
    }
    Ok(job)
}
pub async fn prepare(
    db: &mut SqliteConnection,
    s: &Value,
    pid: &str,
    focus: &str,
    operation: &str,
) -> Result<Value, String> {
    let op = format!("distillation-request:{}:{operation}", str_field(s, "id")?);
    if let Some(old) = get(db, OP_NS, &op).await? {
        if old["projectId"] != pid || old["focus"] != focus {
            return Err("同一操作编号不能创建不同的蒸馏任务".into());
        }
        return view(db, str_field(&old, "id")?).await;
    }
    let p = book::project(db, pid).await?;
    let catalog = book::catalog(db, pid).await?;
    if catalog.is_empty() {
        return Err("参考书没有可读取的章节".into());
    }
    let jid = id();
    let mut job = json!({"id":jid,"projectId":pid,"sessionId":s["id"],"title":p["title"],"focus":focus,
        "status":"awaiting_scope","phase":"reading","createdAt":now(),"version":PROTOCOL_VERSION,
        "catalog":catalog.iter().map(|c|json!({"id":c["id"].to_string(),"title":c["title"],"chars":c["wordCount"]})).collect::<Vec<_>>(),
        "totalChapters":catalog.len(),"totalChars":catalog.iter().map(|c|c["wordCount"].as_u64().unwrap_or(0)).sum::<u64>(),
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0},"error":null});
    save_job(db, &mut job).await?;
    put(
        db,
        OP_NS,
        &op,
        &json!({"id":jid,"projectId":pid,"focus":focus}),
    )
    .await?;
    let mut conversation = session(db, str_field(s, "id")?).await?;
    conversation["messages"].as_array_mut().ok_or("会话消息无效")?.push(json!({"id":format!("distillation:{jid}"),"role":"assistant","kind":"result","text":"请选择蒸馏范围，确认后开始。","distillationId":jid,"runId":s["runId"],"createdAt":now()}));
    save_session(db, &mut conversation).await?;
    view(db, &jid).await
}

/// Every source character belongs to a persisted leaf. Split by capacity (in characters), never
/// truncate a chapter or silently substitute an existing synopsis for prose.
fn windows(text: &str, budget: usize) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let budget = budget.max(1);
    let mut spans = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        let mut paragraph_end = None;
        while end < chars.len() && end - start < budget {
            end += 1;
            if chars[end - 1] == '\n' {
                paragraph_end = Some(end);
            }
        }
        if end < chars.len() {
            end = paragraph_end.filter(|p| *p > start).unwrap_or(end);
        }
        if end == start {
            end += 1;
        }
        spans.push((start, end));
        start = end;
    }
    spans
}
/// Host-assigned paragraph locations. The model selects IDs; it never has to
/// reproduce source characters or calculate Unicode offsets. Existing completed
/// leaves keep their stored evidence IDs and need no migration.
fn source_passages(text: &str, offset: usize) -> Vec<Value> {
    let mut at = offset;
    let mut passages = Vec::new();
    for paragraph in text.split_inclusive('\n') {
        let end = at + paragraph.chars().count();
        if !paragraph.trim().is_empty() {
            passages.push(
                json!({"id":format!("p{}",passages.len()+1),"start":at,"end":end,"text":paragraph}),
            );
        }
        at = end;
    }
    passages
}
async fn enqueue(db: &mut SqliteConnection, jid: &str, node: &Value) -> Result<(), String> {
    let nid = str_field(node, "id")?;
    put(db, NODE_NS, &node_key(jid, nid), node).await?;
    sqlx::query("INSERT INTO agent_outbox(kind,bookId,chapterId,createdAt) VALUES(?,?,?,?)")
        .bind(TASK_KIND)
        .bind(queue_book(jid))
        .bind(nid)
        .bind(now())
        .execute(&mut *db)
        .await
        .map_err(err)?;
    Ok(())
}
/// 一次请求能放多少字的输入（按中文约 1.4 字一个 token 估，留出格式与说明）。
/// 阅读片段另有上限 `SLICE_MAX_CHARS`；归纳层按序列化后的字数与这个预算比较。
fn model_input_budget(model: &Value) -> Result<usize, String> {
    let context = model["maxContext"].as_u64().unwrap_or(32000) as usize;
    let output = model["maxOutputTokens"].as_u64().unwrap_or(4096) as usize;
    let tokens = context.saturating_sub(output).saturating_sub(4096);
    let input_budget = tokens.saturating_mul(14) / 10;
    if input_budget < 1500 {
        return Err("模型上下文扣除输出和分析格式后不足，请调整模型容量".into());
    }
    Ok(input_budget)
}

fn serialized_chars(value: &Value) -> usize {
    value.to_string().chars().count()
}
/// A native scope card owns this planner turn. Older workers sometimes added
/// a second, unlinked ask_author card after it. Supersede that wait without
/// inventing an answer; explicit permission confirmations are never dismissed.
pub(crate) fn reconcile_scope_session(session: &mut Value, job: &Value) -> bool {
    if session["id"] != job["sessionId"] {
        return false;
    }
    let before = session.clone();
    let Some(messages) = session["messages"].as_array_mut() else {
        return false;
    };
    let Some(index) = messages
        .iter()
        .position(|m| m["distillationId"] == job["id"])
    else {
        return false;
    };
    let run = messages[index]["runId"].clone();
    if job["scope"].is_object() {
        messages[index]["text"] = json!(format!(
            "蒸馏范围已确认：第 {}–{} 章。",
            job["scope"]["from"], job["scope"]["to"]
        ));
    }
    for message in messages.iter_mut().skip(index + 1) {
        if message["runId"] == run
            && message["choice"]["status"] == "pending"
            && !message["choice"]["confirmation"].is_string()
        {
            message["choice"]["status"] = json!("superseded");
            message["choice"]["supersededBy"] = json!({"kind":"distillation","id":job["id"]});
        }
    }
    let has_pending = messages.iter().any(|m| m["choice"]["status"] == "pending");
    if session["runId"] == run && session["status"] == "choice" && !has_pending {
        session["status"] = json!("completed");
        session["choiceId"] = Value::Null;
        session["promptDraft"] = Value::Null;
        session["currentTool"] = Value::Null;
        crate::agent_host::settle_references(session);
    }
    *session != before
}
pub(crate) async fn sync_scope_interactions(db: &mut SqliteConnection) -> Result<(), String> {
    for job in list(db, NS, "distillation-job:").await? {
        let Ok(mut session) = session(db, str_field(&job, "sessionId")?).await else {
            continue;
        };
        if reconcile_scope_session(&mut session, &job) {
            save_session(db, &mut session).await?;
        }
    }
    Ok(())
}
fn document_kind(node: &Value) -> &'static str {
    match node["kind"].as_str() {
        Some("leaf") => "distillationLeaf",
        Some("summary") => "distillationDigest",
        _ => "distillationReview",
    }
}
/// Reuse a stored, valid model answer rejected solely for extra commentary.
/// Canonicalization never supplies required fields or repairs evidence IDs.
async fn recover_candidate(
    db: &mut SqliteConnection,
    job: &Value,
    node: &Value,
) -> Result<bool, String> {
    if node["errorCode"] != "invalid_output" || !node["diagnostic"]["parsed"].is_object() {
        return Ok(false);
    }
    let candidate = &node["diagnostic"]["parsed"];
    let projected = crate::agent_contract::project_document(document_kind(node), candidate)?;
    if projected == *candidate
        || crate::agent_contract::validate_document(document_kind(node), &projected).is_err()
    {
        return Ok(false);
    }
    let jid = str_field(job, "id")?;
    let nid = str_field(node, "id")?;
    let run = format!("distill-recovery:{}", id());
    let mut leased = node.clone();
    leased["lease"] = json!(run);
    leased["status"] = json!("running");
    leased["recoveredFromDiagnostic"] = json!(true);
    put(db, NODE_NS, &node_key(jid, nid), &leased).await?;
    let result = finish(
        db,
        &run,
        &json!({"jobId":jid,"id":nid,"result":projected,"modelCalls":0,"usage":{}}),
    )
    .await;
    if result.as_ref().is_ok_and(|r| r["saved"] == true) {
        return Ok(true);
    }
    put(db, NODE_NS, &node_key(jid, nid), node).await?;
    Ok(false)
}
pub async fn start(
    db: &mut SqliteConnection,
    jid: &str,
    mode: &str,
    from: usize,
    to: usize,
    model: &Value,
) -> Result<Value, String> {
    let mut job = load(db, jid).await?;
    if job["status"] != "awaiting_scope" {
        return view(db, jid).await;
    }
    let pid = str_field(&job, "projectId")?.to_owned();
    let catalog = book::catalog(db, &pid).await?;
    // The user approved a concrete catalog. Changed sources need a fresh choice.
    if catalog.len() != number(&job["totalChapters"])
        || catalog
            .iter()
            .zip(job["catalog"].as_array().unwrap())
            .any(|(c, old)| c["id"].to_string() != old["id"].as_str().unwrap_or(""))
    {
        return Err("目录已变化，请重新创建蒸馏任务并选择范围".into());
    }
    let (from, to) = match mode {
        "full" => (1, catalog.len()),
        "range" => (from, to),
        _ => return Err("请选择全书或指定章节范围".into()),
    };
    if from == 0 || from > to || to > catalog.len() {
        return Err("章节范围超出目录".into());
    }
    let input_budget = model_input_budget(model)?;
    queue::ensure_schema(db).await?;
    let mut manifest = Vec::new();
    let mut leaf_ids = Vec::new();
    let mut source_chars = 0;
    for (offset, c) in catalog[from - 1..to].iter().enumerate() {
        let cid = c["id"].to_string();
        let ch = book::chapter(db, &pid, &cid).await?;
        let text = ch["textContent"].as_str().unwrap_or("");
        let revision = book::version_of(&ch);
        let count = text.chars().count();
        source_chars += count;
        manifest.push(json!({"chapterId":cid,"revision":revision,"chars":count,"no":from+offset,"title":ch["title"]}));
        let spans = windows(text, input_budget.min(SLICE_MAX_CHARS));
        // Empty chapters have an explicit zero-length coverage record, no paid call.
        for (part, (start, end)) in spans.iter().enumerate() {
            let nid = format!("leaf-{}-{part}", from + offset);
            leaf_ids.push(nid.clone());
            enqueue(db,jid,&json!({"id":nid,"jobId":jid,"kind":"leaf","status":"pending","level":0,"chapterId":cid,
                "no":from+offset,"title":ch["title"],"sourceRevision":revision,"start":start,"end":end,"attempts":0})).await?;
        }
    }
    if leaf_ids.is_empty() {
        return Err("所选章节没有正文".into());
    }
    job.as_object_mut().unwrap().remove("catalog");
    job["scope"] = json!({"mode":mode,"from":from,"to":to});
    job["manifest"] = json!(manifest);
    job["sourceChars"] = json!(source_chars);
    job["inputBudget"] = json!(input_budget);
    job["modelId"] = model["id"].clone();
    job["layer"] = json!(leaf_ids);
    job["level"] = json!(0);
    job["status"] = json!("running");
    job["startedAt"] = json!(now());
    save_job(db, &mut job).await?;
    if let Ok(mut conversation) = session(db, str_field(&job, "sessionId")?).await {
        if reconcile_scope_session(&mut conversation, &job) {
            save_session(db, &mut conversation).await?;
        }
    }
    view(db, jid).await
}
pub async fn view(db: &mut SqliteConnection, jid: &str) -> Result<Value, String> {
    let mut job = load(db, jid).await?;
    let prefix = format!("\"{jid}:");
    let counts:(i64,i64,i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT COALESCE(SUM(CASE WHEN json_extract(value,'$.kind')='leaf' AND json_extract(value,'$.status')='completed' THEN json_extract(value,'$.end')-json_extract(value,'$.start') ELSE 0 END),0),COALESCE(SUM(json_extract(value,'$.status')='completed'),0),COUNT(*),COALESCE(SUM(json_extract(value,'$.status')='failed'),0),COUNT(DISTINCT CASE WHEN json_extract(value,'$.kind')='leaf' AND json_extract(value,'$.status')!='completed' THEN json_extract(value,'$.chapterId') END),COALESCE(SUM(json_extract(value,'$.usage.input_tokens')),0),COALESCE(SUM(json_extract(value,'$.usage.output_tokens')),0),COALESCE(SUM(json_extract(value,'$.usage.total_tokens')),0) FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=?")
        .bind(NODE_NS).bind(&prefix).bind(&prefix).fetch_one(&mut *db).await.map_err(err)?;
    let total = job["manifest"].as_array().map_or(0, |m| m.len());
    job["doneChapters"] = json!(total.saturating_sub(counts.4 as usize));
    job["selectedChapters"] = json!(total);
    job["coveredChars"] = json!(counts.0);
    job["doneUnits"] = json!(counts.1);
    job["totalUnits"] = json!(counts.2);
    job["failedUnits"] = json!(counts.3);
    job["usage"] =
        json!({"input_tokens":counts.5,"output_tokens":counts.6,"total_tokens":counts.7});
    if job["status"] == "awaiting_scope" {
        job["chapterSizes"] = json!(job["catalog"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| number(&c["chars"]))
            .collect::<Vec<_>>());
    }
    if let Some(map) = job.as_object_mut() {
        for key in ["manifest", "layer", "catalog"] {
            map.remove(key);
        }
    }
    Ok(job)
}
pub async fn list_views(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for job in list(db, NS, "distillation-job:").await? {
        out.push(view(db, str_field(&job, "id")?).await?);
    }
    Ok(out)
}
pub async fn control(db: &mut SqliteConnection, jid: &str, action: &str) -> Result<Value, String> {
    let mut job = load(db, jid).await?;
    match action {
        "pause" if job["status"] == "running" => {
            job["status"] = json!("paused");
            queue::set_index_paused(db, &queue_book(jid), true).await?;
        }
        "resume" if job["status"] == "paused" || job["status"] == "failed" => {
            check_sources(db, &job).await?;
            // Capacity changes apply to pending work; source spans and completed
            // analysis remain bound to their original versions.
            if let Ok(model) =
                crate::agent_service::model_config(db, &job["modelId"].to_string(), "agent_indexer")
                    .await
            {
                job["inputBudget"] = json!(model_input_budget(&model)?);
            }
            for mut n in nodes(db, jid)
                .await?
                .into_iter()
                .filter(|n| n["status"] == "failed")
            {
                if recover_candidate(db, &job, &n).await? {
                    continue;
                }
                // 作者点了"继续"：次数重新数，队列行放回去
                n["status"] = json!("pending");
                n["error"] = Value::Null;
                n["attempts"] = json!(0);
                n["runs"] = json!(0);
                n["failureSignature"] = Value::Null;
                let nid = str_field(&n, "id")?.to_owned();
                put(db, NODE_NS, &node_key(jid, &nid), &n).await?;
                requeue(db, jid, &nid).await?;
            }
            job["status"] = json!("running");
            job["error"] = Value::Null;
            queue::set_index_paused(db, &queue_book(jid), false).await?;
        }
        "cancel" if job["status"] != "completed" => {
            job["status"] = json!("cancelled");
            queue::drop_pending(db, &queue_book(jid)).await?;
        }
        _ => return Err("当前任务不能执行该操作".into()),
    }
    save_job(db, &mut job).await?;
    view(db, jid).await
}
async fn check_sources(db: &mut SqliteConnection, job: &Value) -> Result<(), String> {
    for source in job["manifest"].as_array().into_iter().flatten() {
        let current = book::chapter(
            db,
            str_field(job, "projectId")?,
            str_field(source, "chapterId")?,
        )
        .await?;
        if book::version_of(&current) != source["revision"].as_str().unwrap_or("") {
            return Err("原文已变化，旧分析保留；请按新版本重新创建蒸馏任务".into());
        }
    }
    Ok(())
}
/// 节点给下一层看的样子：阅读片段给出范例（编号、类型、理由，原文由 `summary_input` 补上）；归纳给出写法卡与叙事模式。
fn projection(n: &Value) -> Value {
    if n["kind"] == "verify" {
        json!({"id":n["id"],"review":n["result"]})
    } else {
        json!({"id":n["id"],"range":{"from":n.get("from").unwrap_or(&n["no"]),"to":n.get("to").unwrap_or(&n["no"])},"summary":n["result"]["summary"],"observations":n["result"]["observations"]})
    }
}
/// 范例编号：一处标出的段落以它第一个证据编号为名，写法卡引用它。
fn pick_id(pick: &Value) -> Option<String> {
    pick["evidence"].as_array().and_then(|e| e.first()).and_then(Value::as_str).map(str::to_owned)
}
/// 下一层可以引用的编号：范例编号与其中每个证据编号都算。
fn evidence_ids(result: &Value) -> BTreeSet<String> {
    result["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| strings(&o["evidence"]))
        .collect()
}
/// 一处范例的原文：按证据位置从当前版本读回，连起来。
async fn pick_text(db: &mut SqliteConnection, job: &Value, pick: &Value) -> Result<String, String> {
    let mut parts = Vec::new();
    for eid in strings(&pick["evidence"]) {
        let source = evidence(db, job, &eid).await?;
        parts.push(source["quote"].as_str().unwrap_or("").trim().to_owned());
    }
    Ok(parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n"))
}
async fn evidence(db: &mut SqliteConnection, job: &Value, eid: &str) -> Result<Value, String> {
    let (nid, index) = eid.rsplit_once('#').ok_or("原文证据编号无效")?;
    let node = get(db, NODE_NS, &node_key(str_field(job, "id")?, nid))
        .await?
        .ok_or("原文证据不存在")?;
    if node["kind"] != "leaf" || node["status"] != "completed" {
        return Err("来源阅读尚未完成".into());
    }
    let index = index.parse::<usize>().map_err(err)?;
    let source = node["result"]["evidence"]
        .as_array()
        .and_then(|a| a.get(index))
        .ok_or("证据位置不存在")?;
    let chapter = book::chapter(
        db,
        str_field(job, "projectId")?,
        str_field(&node, "chapterId")?,
    )
    .await?;
    if book::version_of(&chapter) != node["sourceRevision"].as_str().unwrap_or("") {
        return Err("证据原文版本已变化".into());
    }
    let text = chapter["textContent"].as_str().unwrap_or("");
    let start = number(&source["start"]);
    let end = number(&source["end"]);
    // Return the paragraph containing the quote, not another generated summary.
    let chars: Vec<char> = text.chars().collect();
    let quote: String = chars
        .get(start..end)
        .ok_or("证据位置超出当前原文")?
        .iter()
        .collect();
    let left = chars[..start]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1);
    let right = chars[end..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |p| end + p);
    Ok(
        json!({"id":eid,"projectId":job["projectId"],"chapterId":node["chapterId"],"chapterNo":node["no"],"title":node["title"],
        "revision":node["sourceRevision"],"start":start,"end":end,"quote":quote,"context":chars[left..right].iter().collect::<String>()}),
    )
}

/// Slices reference persisted analysis, never copy the source novel into task records.
async fn summary_input(
    db: &mut SqliteConnection,
    jid: &str,
    node: &Value,
) -> Result<Vec<Value>, String> {
    let slices = node["slices"].as_array().cloned().unwrap_or_else(|| {
        strings(&node["children"])
            .iter()
            .map(|id| json!({"id":id}))
            .collect()
    });
    let mut children = Vec::new();
    for slice in slices {
        let n = get(db, NODE_NS, &node_key(jid, str_field(&slice, "id")?))
            .await?
            .ok_or("上游节点缺失")?;
        if n["status"] != "completed" {
            return Err("上游分析未完成".into());
        }
        let mut projected = projection(&n);
        let observations = projected["observations"].as_array().ok_or("上游观察无效")?;
        let from = number(&slice["from"]);
        let to = slice["to"]
            .as_u64()
            .map_or(observations.len(), |v| v as usize);
        if from > to || to > observations.len() {
            return Err("归纳分段范围无效".into());
        }
        let mut selected = observations[from..to].to_vec();
        if n["kind"] == "leaf" {
            // 归纳看的是范例原文，不是上一层的形容词
            let job = load(db, jid).await?;
            for pick in &mut selected {
                let text = pick_text(db, &job, pick).await?;
                pick["id"] = json!(pick_id(pick));
                pick["text"] = json!(text);
            }
        }
        projected["observations"] = json!(selected);
        children.push(projected);
    }
    Ok(children)
}

/// A capacity failure changes the work, not just the prompt: split by serialized
/// input size and reuse the same queue. A single indivisible observation fails
/// visibly; no observation, summary or evidence is silently discarded.
async fn split_summary(
    db: &mut SqliteConnection,
    node: &mut Value,
    input: &Value,
) -> Result<bool, String> {
    let jid = str_field(node, "jobId")?.to_owned();
    let nid = str_field(node, "id")?.to_owned();
    let children = input["children"].as_array().ok_or("归纳输入无效")?;
    let mut slices = node["slices"].as_array().cloned().unwrap_or_else(|| children.iter().map(|c|json!({"id":c["id"],"from":0,"to":c["observations"].as_array().map_or(0,|a|a.len())})).collect());
    let cut = if slices.len() > 1 {
        let sizes: Vec<usize> = children.iter().map(|c| c.to_string().len()).collect();
        let total: usize = sizes.iter().sum();
        let mut left = 0;
        (1..sizes.len())
            .min_by_key(|i| {
                left += sizes[i - 1];
                left.abs_diff(total - left)
            })
            .unwrap()
    } else if slices.len() == 1 {
        let from = number(&slices[0]["from"]);
        let to = number(&slices[0]["to"]);
        if to.saturating_sub(from) < 2 {
            return Ok(false);
        }
        let mid = from + (to - from) / 2;
        let mut right = slices[0].clone();
        slices[0]["to"] = json!(mid);
        right["from"] = json!(mid);
        slices.push(right);
        1
    } else {
        return Ok(false);
    };
    let mut parts = Vec::new();
    for (index, group) in [&slices[..cut], &slices[cut..]].into_iter().enumerate() {
        let part = format!("{nid}-part-{index}");
        let child_ids: Vec<Value> = group.iter().map(|s| s["id"].clone()).collect();
        let first = children
            .iter()
            .find(|c| c["id"] == group[0]["id"])
            .ok_or("归纳分段来源缺失")?;
        let last = children
            .iter()
            .find(|c| c["id"] == group.last().unwrap()["id"])
            .ok_or("归纳分段来源缺失")?;
        enqueue(
            db,
            &jid,
            &json!({"id":part,"jobId":jid,"kind":"summary","status":"pending","level":node["level"],
            "from":first["range"]["from"],"to":last["range"]["to"],"children":child_ids,"slices":group,"attempts":0}),
        )
        .await?;
        parts.push(part);
    }
    node["parts"] = json!(parts);
    node["status"] = json!("waiting");
    node["error"] = Value::Null;
    put(db, NODE_NS, &node_key(&jid, &nid), node).await?;
    sqlx::query("UPDATE agent_outbox SET processedAt=? WHERE kind=? AND bookId=? AND chapterId=? AND processedAt IS NULL")
        .bind(now()).bind(TASK_KIND).bind(queue_book(&jid)).bind(nid).execute(&mut *db).await.map_err(err)?;
    Ok(true)
}

fn collect_parts(node: &mut Value, parts: &[Value]) {
    let summaries: Vec<&str> = parts
        .iter()
        .filter_map(|p| p["result"]["summary"].as_str())
        .collect();
    let observations: Vec<Value> = parts
        .iter()
        .flat_map(|p| {
            p["result"]["observations"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned()
        })
        .collect();
    // These are already validated model results. Combining pages does not impose
    // the per-response string limit on the stored hierarchy.
    node["result"] = json!({"summary":summaries.join("\n\n"),"observations":observations});
    node["status"] = json!("completed");
    node["error"] = Value::Null;
    node["completedAt"] = json!(now());
}
async fn settle_parts(db: &mut SqliteConnection, jid: &str) -> Result<(), String> {
    loop {
        let prefix = format!("\"{jid}:");
        let pending:Vec<String>=sqlx::query_scalar("SELECT value FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=? AND json_extract(value,'$.status')='waiting'")
            .bind(NODE_NS).bind(&prefix).bind(&prefix).fetch_all(&mut *db).await.map_err(err)?;
        let mut changed = false;
        for raw in pending {
            let mut node: Value = serde_json::from_str(&raw).map_err(err)?;
            let ids = strings(&node["parts"]);
            let mut parts = Vec::new();
            for id in &ids {
                let part = get(db, NODE_NS, &node_key(jid, id))
                    .await?
                    .ok_or("分段归纳结果缺失")?;
                if part["status"] != "completed" {
                    break;
                }
                parts.push(part);
            }
            if parts.is_empty() || parts.len() != ids.len() {
                continue;
            }
            collect_parts(&mut node, &parts);
            put(db, NODE_NS, &node_key(jid, str_field(&node, "id")?), &node).await?;
            changed = true;
        }
        if !changed {
            return Ok(());
        }
    }
}
/// 一次核验全部写法卡：模型对着范例原文逐条判断，不再每条回查一次。
async fn begin_verification(
    db: &mut SqliteConnection,
    job: &mut Value,
    root: &Value,
) -> Result<(), String> {
    let jid = str_field(job, "id")?.to_owned();
    job["rootNode"] = root["id"].clone();
    job["phase"] = json!("verifying");
    let notes = root["result"]["observations"].as_array().cloned().unwrap_or_default();
    if notes.is_empty() {
        return Err("已读内容没有归纳出写法卡，分析保留，无法生成文风档案".into());
    }
    enqueue(db,&jid,&json!({"id":"verify","jobId":jid,"kind":"verify","status":"pending","notes":notes,"attempts":0})).await?;
    job["layer"] = json!(["verify"]);
    Ok(())
}

/// 所有阅读片段标出的范例，按原书顺序。
async fn all_picks(db: &mut SqliteConnection, jid: &str) -> Result<Vec<Value>, String> {
    let mut leaves = nodes(db, jid)
        .await?
        .into_iter()
        .filter(|n| n["kind"] == "leaf" && n["status"] == "completed")
        .collect::<Vec<_>>();
    leaves.sort_by_key(|n| (number(&n["no"]), number(&n["start"])));
    let mut picks = Vec::new();
    for leaf in leaves {
        for pick in leaf["result"]["observations"].as_array().into_iter().flatten() {
            let Some(id) = pick_id(pick) else { continue };
            let mut item = pick.clone();
            item["id"] = json!(id);
            item["chapterId"] = leaf["chapterId"].clone();
            item["chapterNo"] = leaf["no"].clone();
            item["chapterTitle"] = leaf["title"].clone();
            item["revision"] = leaf["sourceRevision"].clone();
            picks.push(item);
        }
    }
    Ok(picks)
}

/// 范例文字控制在档案规定的长度内：太长按段落截到上限，不在句中切断。
fn clamp_example(text: &str) -> String {
    let max = crate::agent_style_profile::EXAMPLE_MAX_CHARS;
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut kept = Vec::new();
    let mut count = 0;
    for paragraph in text.split('\n') {
        let len = paragraph.chars().count();
        if count + len > max && !kept.is_empty() {
            break;
        }
        if count + len > max {
            kept.push(paragraph.chars().take(max).collect::<String>());
            break;
        }
        kept.push(paragraph.to_owned());
        count += len;
    }
    kept.join("\n")
}

/// 从全部范例里选进档案的：写法卡引用的必选，其余每类最多 `EXAMPLES_PER_KIND` 段，优先不同章节。
fn select_profile_examples(picks: &[Value], required: &BTreeSet<String>) -> Vec<Value> {
    let mut chosen: Vec<Value> = picks.iter().filter(|p| required.contains(p["id"].as_str().unwrap_or(""))).cloned().collect();
    for kind in crate::agent_style_profile::SCENE_KINDS {
        let candidates: Vec<Value> = picks.iter().filter(|p| p["kind"] == *kind).cloned().collect();
        // 先挑没出现过的章节，再补同章的
        for pass in 0..2 {
            for pick in &candidates {
                let same_kind: Vec<&Value> = chosen.iter().filter(|p| p["kind"] == *kind).collect();
                if same_kind.len() >= crate::agent_style_profile::EXAMPLES_PER_KIND {
                    break;
                }
                if chosen.iter().any(|c| c["id"] == pick["id"]) {
                    continue;
                }
                let seen_chapter = same_kind.iter().any(|p| p["chapterId"] == pick["chapterId"]);
                if pass == 0 && seen_chapter {
                    continue;
                }
                chosen.push(pick.clone());
            }
        }
    }
    chosen.sort_by_key(|p| (number(&p["chapterNo"]), p["id"].as_str().unwrap_or("").to_owned()));
    chosen
}

/// 程序编档：范例库 + 指纹 + 写法卡 + 叙事模式，写成文风技能候选。不调模型。
async fn compile_profile(db: &mut SqliteConnection, job: &mut Value, review: &Value) -> Result<Value, String> {
    let jid = str_field(job, "id")?.to_owned();
    check_sources(db, job).await?;
    let root = get(db, NODE_NS, &node_key(&jid, str_field(job, "rootNode")?)).await?.ok_or("归纳结果缺失")?;
    let candidates = root["result"]["observations"].as_array().cloned().unwrap_or_default();
    let mut notes = Vec::new();
    for item in review["items"].as_array().into_iter().flatten() {
        let index = number(&item["index"]);
        let Some(candidate) = candidates.get(index) else { continue };
        let text = match item["verdict"].as_str() {
            Some("keep") => candidate["text"].as_str().unwrap_or("").to_owned(),
            Some("revise") => item["text"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(candidate["text"].as_str().unwrap_or("")).to_owned(),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        notes.push(json!({"text":text.trim(),"evidence":candidate["evidence"],"reason":item["reason"]}));
    }
    if notes.is_empty() {
        return Err("核验没有留下任何写法卡；阅读记录已保留，可查看依据后扩大范围或调整分析目标".into());
    }
    let picks = all_picks(db, &jid).await?;
    let required: BTreeSet<String> = notes.iter().flat_map(|n| strings(&n["evidence"])).collect();
    let selected = select_profile_examples(&picks, &required);
    let mut examples = Vec::new();
    let mut source_text = String::new();
    for chapter in job["manifest"].as_array().into_iter().flatten() {
        let body = book::chapter(db, str_field(job, "projectId")?, str_field(chapter, "chapterId")?).await?;
        source_text.push_str(body["textContent"].as_str().unwrap_or(""));
        source_text.push('\n');
    }
    for pick in &selected {
        let text = clamp_example(&pick_text(db, job, pick).await?);
        if text.trim().is_empty() {
            continue;
        }
        let first = evidence(db, job, &strings(&pick["evidence"])[0]).await?;
        examples.push(json!({"id":pick["id"],"kind":pick["kind"],"why":pick["why"],"text":text,
            "source":{"type":"reference","projectId":job["projectId"],"chapterId":pick["chapterId"],"no":pick["chapterNo"],"title":pick["chapterTitle"],
                "start":first["start"],"revision":pick["revision"]}}));
    }
    let example_ids: BTreeSet<String> = examples.iter().filter_map(|e| e["id"].as_str().map(str::to_owned)).collect();
    let notes: Vec<Value> = notes.into_iter().map(|n| {
        let ids: Vec<String> = strings(&n["evidence"]).into_iter().filter(|id| example_ids.contains(id)).collect();
        json!({"text":n["text"],"exampleId":ids.first().cloned().unwrap_or_default(),"exampleIds":ids})
    }).filter(|n| !n["exampleIds"].as_array().unwrap().is_empty()).collect();
    let narrative: Vec<Value> = root["result"]["summary"].as_str().unwrap_or("").lines()
        .map(|l| l.trim().trim_start_matches(['-', '•', '·']).trim())
        .filter(|l| !l.is_empty())
        .map(|l| json!({"text":l}))
        .collect();
    let title = job["title"].as_str().unwrap_or("参考书").to_owned();
    let scope = &job["scope"];
    let fingerprint = crate::agent_style::style_fingerprint(&source_text);
    let chapters: Vec<Value> = job["manifest"].as_array().into_iter().flatten()
        .map(|c| json!({"chapterId":c["chapterId"],"no":c["no"],"title":c["title"],"revision":c["revision"]})).collect();
    let profile = json!({"version":2,"source":{"type":"reference","projectId":job["projectId"],"title":title,"scope":scope,"chapters":chapters,
            "sourceVersion":version(&job["manifest"])},
        "fingerprint":fingerprint,"examples":examples,"notes":notes,"narrative":narrative,"jobId":jid});
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for e in &examples { *kinds.entry(e["kind"].as_str().unwrap_or("").to_owned()).or_default() += 1; }
    let mut lines = vec![
        "## 这份档案是什么".to_owned(),
        format!("《{title}》第 {}–{} 章的文风档案。写作时平台按场景类型把 references/examples.json 里的原文范例交给写作模型；下面的写法卡与叙事模式只给规划与评审看，不给写作模型，也不含任何数字。", scope["from"], scope["to"]),
        String::new(),
        "## 写法卡".to_owned(),
    ];
    for note in &notes {
        lines.push(format!("- {}（范例 {}）", note["text"].as_str().unwrap_or(""), note["exampleIds"].as_array().unwrap().iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、")));
    }
    if !narrative.is_empty() {
        lines.push(String::new());
        lines.push("## 叙事模式".to_owned());
        for item in &narrative { lines.push(format!("- {}", item["text"].as_str().unwrap_or(""))); }
    }
    lines.push(String::new());
    lines.push("## 范例库".to_owned());
    lines.push(format!("共 {} 段：{}。原文与来源位置在 references/examples.json。", examples.len(),
        kinds.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join("、")));
    let coverage = json!({"jobId":jid,"projectId":job["projectId"],"title":title,"scope":scope,
        "chapters":chapters.len(),"sourceChars":job["sourceChars"],"sourceVersion":version(&job["manifest"]),
        "validation":"原文覆盖与范例位置经宿主核验；写法卡经一次对照范例核验。文学效果和新场景试写未验证。"});
    let draft = json!({"id":"","kind":"style","revision":Value::Null,
        "name":format!("《{title}》文风档案"),
        "description":format!("从《{title}》第 {}–{} 章提炼：{} 段原文范例、{} 条写法卡、{} 条叙事模式", scope["from"], scope["to"], examples.len(), notes.len(), narrative.len()),
        "instructions":lines.join("\n"),
        "files":[{"path":crate::agent_style_profile::EXAMPLES_PATH,"content":serde_json::to_string_pretty(&profile).map_err(err)?},
                 {"path":"references/coverage.json","content":serde_json::to_string_pretty(&coverage).map_err(err)?}]});
    let a = crate::agent_skill_authoring::prepare_background(db, str_field(job, "sessionId")?, &jid, &draft).await?;
    job["artifactId"] = a["id"].clone();
    job["status"] = json!("completed");
    job["finishedAt"] = json!(now());
    let mut s = session(db, str_field(job, "sessionId")?).await?;
    if !s["messages"].as_array().into_iter().flatten().any(|m| m["artifactId"] == a["id"]) {
        s["messages"].as_array_mut().ok_or("会话消息无效")?.push(json!({"id":format!("distillation:{jid}:result"),"role":"assistant","kind":"result",
            "text":format!("已完成所选范围的阅读、归纳与核验：{} 段原文范例、{} 条写法卡。文风档案候选可查看和编辑，保存后绑定到作品即可按它写作。", examples.len(), notes.len()),
            "artifactId":a["id"],"createdAt":now()}));
        save_session(db, &mut s).await?;
    }
    Ok(a)
}

/// Advance only after every node in the current layer has a committed result.
/// Fan-in is driven by actual serialized input size, not a fixed chapter count.
async fn advance(db: &mut SqliteConnection, job: &mut Value) -> Result<(), String> {
    let jid = str_field(job, "id")?.to_owned();
    let mut layer = Vec::new();
    let keys = strings(&job["layer"])
        .iter()
        .map(|nid| json!(node_key(&jid, nid)).to_string())
        .collect::<Vec<_>>();
    let unfinished:i64=sqlx::query_scalar("SELECT COUNT(*) FROM desktop_records WHERE namespace=? AND key IN (SELECT value FROM json_each(?)) AND json_extract(value,'$.status')!='completed'")
        .bind(NODE_NS).bind(serde_json::to_string(&keys).map_err(err)?).fetch_one(&mut *db).await.map_err(err)?;
    if unfinished > 0 {
        return Ok(());
    }
    for nid in strings(&job["layer"]) {
        let n = get(db, NODE_NS, &node_key(&jid, &nid))
            .await?
            .ok_or("蒸馏节点缺失")?;
        if n["status"] != "completed" {
            return Ok(());
        }
        layer.push(n);
    }
    if layer.is_empty() {
        return Ok(());
    }
    let phase = job["phase"].as_str().unwrap_or("reading").to_owned();
    if phase == "reading" || phase == "summarizing" {
        if layer.len() == 1 {
            begin_verification(db, job, &layer[0]).await?;
        } else {
            let budget = number(&job["inputBudget"]);
            let level = number(&job["level"]) + 1;
            let mut groups: Vec<Vec<String>> = Vec::new();
            let mut group = Vec::new();
            let mut bytes = 0;
            for n in &layer {
                // 阅读片段给归纳看的是范例原文，按范例字数估大小
                let size = serialized_chars(&projection(n)) + if n["kind"] == "leaf" {
                    n["result"]["observations"].as_array().map_or(0, |a| a.len()) * crate::agent_style_profile::EXAMPLE_MAX_CHARS
                } else { 0 };
                if bytes + size > budget && !group.is_empty() {
                    groups.push(std::mem::take(&mut group));
                    bytes = 0;
                }
                bytes += size;
                group.push(str_field(n, "id")?.to_owned());
            }
            if !group.is_empty() {
                groups.push(group);
            }
            if groups.len() == layer.len() && phase == "reading" {
                groups = vec![strings(&job["layer"])];
            } else if groups.len() == layer.len() {
                // No further reduction fits. Preserve the partitioned hierarchy
                // and verify every observation instead of dropping findings or
                // repeatedly asking the model to compress the same oversized input.
                let mut root = json!({"id":format!("summary-collection-{level}"),"jobId":jid,"kind":"summary",
                    "level":level,"parts":job["layer"],"children":job["layer"],"attempts":0});
                collect_parts(&mut root, &layer);
                put(db, NODE_NS, &node_key(&jid, str_field(&root, "id")?), &root).await?;
                begin_verification(db, job, &root).await?;
                return save_job(db, job).await;
            }
            let mut next = Vec::new();
            for (i, children) in groups.into_iter().enumerate() {
                if children.len() == 1 {
                    next.push(children[0].clone());
                    continue;
                }
                let nid = format!("summary-{level}-{i}");
                next.push(nid.clone());
                let first = layer.iter().find(|n| n["id"] == children[0]).unwrap();
                let last = layer
                    .iter()
                    .find(|n| n["id"] == *children.last().unwrap())
                    .unwrap();
                let from = first.get("from").unwrap_or(&first["no"]);
                let to = last.get("to").unwrap_or(&last["no"]);
                enqueue(db,&jid,&json!({"id":nid,"jobId":jid,"kind":"summary","status":"pending","level":level,"from":from,"to":to,"children":children,"attempts":0})).await?;
            }
            job["layer"] = json!(next);
            job["level"] = json!(level);
            job["phase"] = json!("summarizing");
        }
    } else if phase == "verifying" {
        let review = layer[0]["result"].clone();
        job["reviewNodes"] = job["layer"].clone();
        job["phase"] = json!("compiling");
        compile_profile(db, job, &review).await?;
    }
    save_job(db, job).await
}

pub async fn next_batch(db: &mut SqliteConnection, run: &str) -> Result<Vec<Value>, String> {
    for mut job in list(db, NS, "distillation-job:")
        .await?
        .into_iter()
        .filter(|j| j["status"] == "running")
    {
        if session(db, str_field(&job, "sessionId")?).await.is_err() {
            queue::drop_pending(db, &queue_book(str_field(&job, "id")?)).await?;
            job["status"] = json!("cancelled");
            save_job(db, &mut job).await?;
            continue;
        }
        settle_parts(db, str_field(&job, "id")?).await?;
        if settle_failures(db, &mut job).await? {
            continue;
        }
        let advance_result = if job["version"].as_u64() != Some(PROTOCOL_VERSION) {
            Err("蒸馏协议已升级，旧结果保留；请重新创建任务".to_owned())
        } else {
            advance(db, &mut job).await
        };
        if let Err(e) = advance_result {
            job["status"] = json!("failed");
            job["error"] = json!(e);
            queue::set_index_paused(db, &queue_book(str_field(&job, "id")?), true).await?;
            save_job(db, &mut job).await?;
        }
    }
    let mut out = Vec::new();
    for group in queue::claim_in(db, run, WORKERS, 0, Some(queue::Lane::Distillation)).await? {
        let jid = group
            .book_id
            .strip_prefix("distillation:")
            .ok_or("任务队列编号无效")?;
        let job = load(db, jid).await?;
        if job["status"] != "running" {
            queue::release_group(db, run, &group).await?;
            continue;
        }
        let nid = group.chapter_id.as_deref().ok_or("任务节点编号缺失")?;
        let mut node = get(db, NODE_NS, &node_key(jid, nid))
            .await?
            .ok_or("任务节点不存在")?;
        if node["status"] == "completed" {
            queue::complete(db, run, &group, None).await?;
            continue;
        }
        let mut payload = json!({"jobId":jid,"id":nid,"kind":node["kind"],"title":job["title"],"focus":job["focus"],"inputBudget":job["inputBudget"]});
        let input = build_input(db, &job, &node).await;
        match input {
            Ok(input) => {
                if node["kind"] == "summary"
                    && serialized_chars(&input["children"]) > number(&job["inputBudget"])
                {
                    if !split_summary(db, &mut node, &input).await? {
                        fail(
                            db,
                            jid,
                            nid,
                            "单条观察及其语境超过模型输入容量，请提高模型容量后继续",
                            "context_budget",
                            false,
                        )
                        .await?;
                    }
                    continue;
                }
                payload["input"] = input;
                payload["outputMode"] = node["outputMode"].clone();
                // Old quote-copy errors do not describe the new ID-selection contract.
                // Keep their diagnostics locally, but do not feed obsolete repair
                // instructions into a resumed analysis.
                payload["feedback"] = if node["kind"] != "leaf"
                    || node["inputContract"] == "passages-v1"
                {
                    json!({"code":node["errorCode"],"detail":node["detail"],"issues":node["issues"]})
                } else {
                    Value::Null
                };
                if node["kind"] == "leaf" {
                    node["inputContract"] = json!("passages-v1");
                }
                node["lease"] = json!(run);
                node["status"] = json!("running");
                node["runs"] = json!(number(&node["runs"]) + 1);
                node["queueMaxId"] = json!(group.max_id);
                put(db, NODE_NS, &node_key(jid, nid), &node).await?;
                out.push(payload);
            }
            Err(e) => {
                queue::release_group(db, run, &group).await?;
                fail(db, jid, nid, &e, "source_changed", false).await?;
            }
        }
    }
    Ok(out)
}
async fn build_input(
    db: &mut SqliteConnection,
    job: &Value,
    node: &Value,
) -> Result<Value, String> {
    let jid = str_field(job, "id")?;
    match node["kind"].as_str().unwrap_or("") {
        "leaf" => {
            let ch = book::chapter(
                db,
                str_field(job, "projectId")?,
                str_field(node, "chapterId")?,
            )
            .await?;
            if book::version_of(&ch) != node["sourceRevision"].as_str().unwrap_or("") {
                return Err("章节原文已变化，请重新创建蒸馏任务".into());
            }
            let text = ch["textContent"]
                .as_str()
                .unwrap_or("")
                .chars()
                .skip(number(&node["start"]))
                .take(number(&node["end"]) - number(&node["start"]))
                .collect::<String>();
            Ok(
                json!({"chapterNo":node["no"],"chapterTitle":node["title"],"start":node["start"],"end":node["end"],"passages":source_passages(&text,number(&node["start"]))}),
            )
        }
        "summary" => Ok(
            json!({"children":summary_input(db, jid, node).await?, "scope":job["scope"],
            "totalChapters":job["manifest"].as_array().map_or(0,|a|a.len())}),
        ),
        "verify" => {
            // 写法卡与它们引用的范例原文一起给模型，一次核验
            let notes = node["notes"].as_array().cloned().unwrap_or_default();
            let wanted: BTreeSet<String> = notes.iter().flat_map(|n| strings(&n["evidence"])).collect();
            let mut examples = Vec::new();
            for pick in all_picks(db, jid).await? {
                let id = pick["id"].as_str().unwrap_or("").to_owned();
                let referenced = wanted.contains(&id) || strings(&pick["evidence"]).iter().any(|e| wanted.contains(e));
                if !referenced {
                    continue;
                }
                let text = pick_text(db, job, &pick).await?;
                examples.push(json!({"id":id,"kind":pick["kind"],"chapterNo":pick["chapterNo"],"text":text}));
            }
            let listed: Vec<Value> = notes.iter().enumerate().map(|(i, n)| json!({"index":i,"text":n["text"],"evidence":n["evidence"]})).collect();
            Ok(json!({"notes":listed,"examples":examples,"scope":job["scope"]}))
        }
        _ => Err("未知蒸馏任务阶段".into()),
    }
}
/// 作者看得懂的失败说明；技术细节另存在片段的 detail 里
fn friendly_error(code: &str, detail: &str, attempts: usize) -> String {
    let detail = detail.split("（结束原因：").next().unwrap_or(detail);
    let tries = if attempts > 0 {
        format!("本轮已请求模型 {attempts} 次。")
    } else {
        String::new()
    };
    match code {
        "invalid_output" | "distillation_evidence_invalid" => {
            format!("{detail}。{tries}已保留完成结果，修正后可继续。")
        }
        "transient_output" | "empty_output" => {
            format!("模型未返回有效结果。{tries}已暂停自动重试，可稍后继续。")
        }
        "process" => format!("蒸馏进程中断（{detail}），已保留完成结果，点「继续」再试。"),
        "source_changed" => detail.to_owned(),
        "context_budget" | "output_limit" => {
            format!("{detail}。{tries}已保留完成结果，请调整分析模型容量后继续。")
        }
        _ => format!("{detail}，点「继续」再试。{tries}"),
    }
}
/// 片段的队列行：自动重试与"继续"都要它重新可领
async fn requeue(db: &mut SqliteConnection, jid: &str, nid: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO agent_outbox(kind,bookId,chapterId,createdAt) SELECT ?,?,?,? WHERE NOT EXISTS (SELECT 1 FROM agent_outbox WHERE kind=? AND bookId=? AND chapterId=? AND processedAt IS NULL)")
        .bind(TASK_KIND).bind(queue_book(jid)).bind(nid).bind(now())
        .bind(TASK_KIND).bind(queue_book(jid)).bind(nid).execute(&mut *db).await.map_err(err)?;
    Ok(())
}
/// 片段失败：值得重试且没用完次数就放回待处理，队列行留着下次再领（本轮结束时释放租约）；
/// 否则记为失败并把队列行结掉，其他片段照跑，等没有可跑的片段了才把任务停为"待重试"（见 settle_failures）。
async fn fail(
    db: &mut SqliteConnection,
    jid: &str,
    nid: &str,
    error: &str,
    code: &str,
    retry: bool,
) -> Result<(), String> {
    let Some(mut n) = get(db, NODE_NS, &node_key(jid, nid)).await? else {
        return Ok(());
    };
    let attempts = number(&n["attempts"]);
    let signature = version(
        &json!({"code":code,"issues":n["issues"].as_array().map(|issues| issues.iter().map(|i|json!({"path":i["path"],"rule":i["rule"],"expected":i["expected"]})).collect::<Vec<_>>()),
        "detail":if n["issues"].as_array().is_some_and(|a|!a.is_empty()){ "" }else{error}}),
    );
    let repeated = n["failureSignature"] == signature;
    n["failureSignature"] = json!(signature);
    n["detail"] = json!(error);
    n["errorCode"] = json!(code);
    // 租约留着：同一进程重放 finish 时仍认得出是同一次（settledRun），下次领取会换新租约
    if retry && !repeated && attempts < MAX_NODE_ATTEMPTS && number(&n["runs"]) < MAX_NODE_ATTEMPTS
    {
        n["status"] = json!("pending");
        n["error"] = Value::Null;
        n["lastError"] = json!(friendly_error(code, error, attempts));
        put(db, NODE_NS, &node_key(jid, nid), &n).await?;
        return requeue(db, jid, nid).await;
    }
    n["status"] = json!("failed");
    n["error"] = json!(friendly_error(code, error, attempts));
    put(db, NODE_NS, &node_key(jid, nid), &n).await?;
    sqlx::query("UPDATE agent_outbox SET processedAt=? WHERE kind=? AND bookId=? AND chapterId=? AND processedAt IS NULL")
        .bind(now()).bind(TASK_KIND).bind(queue_book(jid)).bind(nid).execute(&mut *db).await.map_err(err)?;
    Ok(())
}
/// 还在跑的任务：没有待处理、处理中的片段，却有失败的片段，就停为"待重试"并带上第一个失败原因。
async fn settle_failures(db: &mut SqliteConnection, job: &mut Value) -> Result<bool, String> {
    let jid = str_field(job, "id")?.to_owned();
    let all = nodes(db, &jid).await?;
    if all
        .iter()
        .any(|n| matches!(n["status"].as_str(), Some("pending" | "running")))
    {
        return Ok(false);
    }
    let Some(failed) = all.iter().find(|n| n["status"] == "failed") else {
        return Ok(false);
    };
    job["status"] = json!("failed");
    job["error"] = failed["error"].clone();
    queue::set_index_paused(db, &queue_book(&jid), true).await?;
    save_job(db, job).await?;
    Ok(true)
}
pub async fn finish(db: &mut SqliteConnection, run: &str, args: &Value) -> Result<Value, String> {
    let jid = str_field(args, "jobId")?;
    let nid = str_field(args, "id")?;
    let job = load(db, jid).await?;
    let mut n = get(db, NODE_NS, &node_key(jid, nid))
        .await?
        .ok_or("节点不存在")?;
    if n["status"] == "completed" {
        return Ok(json!({"saved":true,"replayed":true}));
    }
    if n["lease"] != run
        || !["running", "paused", "failed"]
            .iter()
            .any(|s| job["status"] == *s)
    {
        return Err("节点租约或任务状态已变化".into());
    }
    if n["settledRun"] == run {
        return Ok(json!({"saved":true,"replayed":true}));
    }
    if let Some(error) = args["error"].as_str() {
        n["settledRun"] = json!(run);
        n["issues"] = args["issues"].clone();
        n["diagnostic"] = args["diagnostic"].clone();
        n["errorCode"] = args["code"].clone();
        n["detail"] = json!(error);
        record_usage(&mut n, args);
        put(db, NODE_NS, &node_key(jid, nid), &n).await?;
        let code = args["code"].as_str().unwrap_or("");
        if matches!(code, "output_limit" | "context_budget") && n["kind"] == "summary" {
            let input = build_input(db, &job, &n).await?;
            if split_summary(db, &mut n, &input).await? {
                return Ok(json!({"recorded":true,"partitioned":true}));
            }
        }
        let retry = RETRYABLE_CODES.contains(&code) || args["retryable"] == true;
        fail(db, jid, nid, error, code, retry).await?;
        return Ok(json!({"recorded":true}));
    }
    let input = build_input(db, &job, &n).await?;
    let mut result = crate::agent_contract::project_document(document_kind(&n), &args["result"])?;
    crate::agent_contract::validate_document(document_kind(&n), &result)?;
    if n["kind"] == "leaf" {
        let passages: BTreeMap<&str, &Value> = input["passages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["id"].as_str().map(|id| (id, p)))
            .collect();
        let mut proof = Vec::new();
        let mut used = BTreeMap::new();
        for (i, observation) in result["observations"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut ids = Vec::new();
            for source_id in strings(&observation["sourceIds"]) {
                let Some(source) = passages.get(source_id.as_str()) else {
                    return Ok(evidence_error(&format!("第 {} 条观察的 sourceIds 引用了不存在的片段 {source_id}，请选择 passages 中的编号",i+1)));
                };
                let index = *used.entry(source_id).or_insert_with(|| {
                    let index = proof.len();
                    proof.push(json!({"start":source["start"],"end":source["end"]}));
                    index
                });
                let eid = format!("{nid}#{index}");
                if !ids.contains(&eid) {
                    ids.push(eid);
                }
            }
            observation.as_object_mut().unwrap().remove("sourceIds");
            observation["evidence"] = json!(ids);
        }
        result["evidence"] = json!(proof);
    } else if n["kind"] == "summary" {
        let allowed: BTreeSet<String> = input["children"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(evidence_ids)
            .collect();
        for o in result["observations"].as_array().into_iter().flatten() {
            let ids = strings(&o["evidence"]);
            if ids.is_empty() || ids.iter().any(|id| !allowed.contains(id)) {
                return Ok(evidence_error(
                    "归纳结论引用了输入范围外的证据，请使用输入中的证据编号",
                ));
            }
        }
    }
    if n["kind"] == "verify" {
        let total = n["notes"].as_array().map_or(0, |a| a.len());
        for item in result["items"].as_array().into_iter().flatten() {
            if number(&item["index"]) >= total {
                return Ok(evidence_error("核验结果引用了不存在的写法卡序号，请按输入 notes 的 index 返回"));
            }
            if item["verdict"] == "revise" && item["text"].as_str().unwrap_or("").trim().is_empty() {
                return Err("标为修订的写法卡必须给出改后的文字".into());
            }
        }
    }
    n["settledRun"] = json!(run);
    n["result"] = result;
    n["status"] = json!("completed");
    n["error"] = Value::Null;
    n["completedAt"] = json!(now());
    record_usage(&mut n, args);
    put(db, NODE_NS, &node_key(jid, nid), &n).await?;
    let group = queue::OutboxGroup {
        kind: TASK_KIND.into(),
        book_id: queue_book(jid),
        chapter_id: Some(nid.into()),
        max_id: n["queueMaxId"].as_i64().unwrap_or(0),
        count: 1,
    };
    queue::complete(db, run, &group, None).await?;
    Ok(json!({"saved":true}))
}
fn evidence_error(message: &str) -> Value {
    crate::agent_tool_boundary::failure(
        "distillation_evidence_invalid",
        crate::agent_tool_boundary::FailureKind::Reference,
        message,
        true,
        "",
        json!({}),
    )
}
fn record_usage(node: &mut Value, args: &Value) {
    if let Some(mode) = args["outputMode"]
        .as_str()
        .filter(|m| matches!(*m, "tool" | "json" | "json_object"))
    {
        node["outputMode"] = json!(mode);
    }
    let calls = args["modelCalls"].as_u64().unwrap_or(1);
    node["modelCalls"] = json!(node["modelCalls"].as_u64().unwrap_or(0) + calls);
    node["attempts"] = json!(node["attempts"].as_u64().unwrap_or(0) + calls);
    if !node["usage"].is_object() {
        node["usage"] = json!({});
    }
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        node["usage"][key] = json!(
            node["usage"][key].as_u64().unwrap_or(0) + args["usage"][key].as_u64().unwrap_or(0)
        );
    }
}

pub async fn read_results(
    db: &mut SqliteConnection,
    jid: &str,
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    let prefix = format!("\"{jid}:");
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=?",
    )
    .bind(NODE_NS)
    .bind(&prefix)
    .bind(&prefix)
    .fetch_one(&mut *db)
    .await
    .map_err(err)?;
    let raw:Vec<String>=sqlx::query_scalar("SELECT value FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=? ORDER BY COALESCE(json_extract(value,'$.level'),0),COALESCE(json_extract(value,'$.no'),0),COALESCE(json_extract(value,'$.start'),0),key LIMIT ? OFFSET ?")
        .bind(NODE_NS).bind(&prefix).bind(&prefix).bind(limit as i64).bind(offset as i64).fetch_all(&mut *db).await.map_err(err)?;
    let rows = raw
        .iter()
        .map(|r| serde_json::from_str::<Value>(r).map_err(err))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"job":view(db,jid).await?,"total":total,"nodes":rows}))
}
pub async fn read_node(db: &mut SqliteConnection, jid: &str, nid: &str) -> Result<Value, String> {
    get(db, NODE_NS, &node_key(jid, nid))
        .await?
        .ok_or("节点不属于本次蒸馏任务".into())
}

pub async fn read_evidence(
    db: &mut SqliteConnection,
    jid: &str,
    eid: &str,
) -> Result<Value, String> {
    let job = load(db, jid).await?;
    evidence(db, &job, eid).await
}

/// Headless and desktop share the same claimed tasks and RPC persistence.
pub fn run_batch(
    root: &Path,
    command: impl FnOnce() -> Result<std::process::Command, String>,
    cancelled: &std::sync::atomic::AtomicBool,
    on_change: impl Fn(&Value),
    on_pid: impl Fn(u32),
) -> Result<bool, String> {
    use crate::agent_worker_pool as pool;
    let run = format!("distill:{}", id());
    let jobs = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let jobs = next_batch(&mut tx, &run).await?;
        tx.commit().await.map_err(err)?;
        Ok::<_, String>(jobs)
    })?;
    if jobs.is_empty() {
        return Ok(false);
    }
    let result = (|| {
        let gateway = crate::model_gateway::endpoint(root)?;
        let models = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(root).await?;
            let mut out = serde_json::Map::new();
            for task in &jobs {
                let jid = str_field(task, "jobId")?;
                if out.contains_key(jid) {
                    continue;
                }
                let job = load(&mut db, jid).await?;
                let model = crate::agent_service::model_config(
                    &mut db,
                    &job["modelId"].to_string(),
                    "agent_indexer",
                )
                .await?;
                out.insert(
                    jid.to_owned(),
                    crate::model_gateway::worker_model(&gateway, &model),
                );
            }
            Ok::<_, String>(Value::Object(out))
        })?;
        let mut resident = pool::acquire(
            &format!("distill:{}", root.display()),
            &pool::log_dir(root),
            command,
        )?;
        on_pid(resident.pid);
        let payload = json!({"action":"distill","models":models,"jobs":jobs});
        let mut worker_error = None;
        let end = pool::run_turn(
            &mut resident,
            &payload,
            |event| {
                if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("蒸馏已停止".into());
                }
                let args = &event["params"];
                if event["method"] != "distillation_finish"
                    || !jobs
                        .iter()
                        .any(|j| j["jobId"] == args["jobId"] && j["id"] == args["id"])
                {
                    return Err("蒸馏进程无权调用该操作".into());
                }
                let out = crate::blocking::run(async {
                    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
                    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
                    let out = finish(&mut tx, &run, args).await?;
                    tx.commit().await.map_err(err)?;
                    Ok::<_, String>(out)
                })?;
                let v = crate::blocking::run(async {
                    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
                    view(&mut db, str_field(args, "jobId")?).await
                })?;
                on_change(&v);
                Ok(out)
            },
            |event| {
                if event["event"] == "failed" {
                    worker_error = Some(
                        event["message"]
                            .as_str()
                            .unwrap_or("蒸馏进程失败")
                            .to_owned(),
                    );
                }
                Ok(())
            },
        );
        match end {
            Ok(pool::TurnEnd::Finished) => {
                pool::release(resident);
                if let Some(e) = worker_error {
                    Err(e)
                } else {
                    Ok(())
                }
            }
            Ok(pool::TurnEnd::Crashed(e)) => {
                pool::discard(resident);
                Err(format!("蒸馏进程中断：{e}"))
            }
            Err(e) => {
                pool::discard(resident);
                Err(e)
            }
        }
    })();
    crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        queue::release(&mut tx, &run).await?;
        if let Err(e) = &result {
            if !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                for task in &jobs {
                    let n = get(
                        &mut tx,
                        NODE_NS,
                        &node_key(str_field(task, "jobId")?, str_field(task, "id")?),
                    )
                    .await?;
                    if n.is_some_and(|n| n["status"] != "completed") {
                        fail(
                            &mut tx,
                            str_field(task, "jobId")?,
                            str_field(task, "id")?,
                            e,
                            "process",
                            true,
                        )
                        .await?;
                    }
                }
            }
        }
        tx.commit().await.map_err(err)
    })?;
    result.map(|_| true)
}
