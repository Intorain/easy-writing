//! 每日工作备注：每天做了什么由平台里已有的事实推出来（码字统计、对话、成果、章节改动），
//! 模型只能记"工作备注"（作者交代的待办、约定好下次要做的事），每条带会话与入口，作者可删。
//! 备注记在 `journal:<日期>:<编号>`；日期按本机时区。

use crate::agent_store::{delete_by_prefix, get, id, list, now, put, str_field, NS};
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

const STATS_NS: &str = "app-settings";
const STATS_KEY: &str = "ew-local-write-stats";
/// 记忆上下文里带最近几天的备注
const CONTEXT_DAYS: i64 = 3;
const CONTEXT_NOTES: usize = 12;

pub fn local_date(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| format!("{:04}-{:02}-{:02}", t.year(), t.month(), t.day()))
        .unwrap_or_default()
}

pub fn today() -> String {
    local_date(now())
}

/// 本机时区某一天的毫秒范围 [起, 止)
fn day_range(date: &str) -> Result<(i64, i64), String> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "日期格式应为 YYYY-MM-DD".to_owned())?;
    let start_of = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.timestamp_millis())
            .ok_or_else(|| "日期无法换算".to_owned())
    };
    Ok((start_of(day)?, start_of(day.succ_opt().ok_or("日期越界")?)?))
}

fn note_key(date: &str, note_id: &str) -> String {
    format!("journal:{date}:{note_id}")
}

/// 模型工具 work_note：记一条今天的工作备注；同一会话同一操作编号重复调用只记一次
pub async fn record(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    let text = str_field(args, "content")?.trim().to_owned();
    if text.is_empty() {
        return Err("备注内容不能为空".into());
    }
    let date = today();
    let operation = args["operation_id"].as_str().unwrap_or("");
    if !operation.is_empty() {
        if let Some(existing) = list(db, NS, &format!("journal:{date}:")).await?.into_iter()
            .find(|n| n["sessionId"] == s["id"] && n["operationId"] == operation)
        {
            return Ok(json!({"id":existing["id"],"date":date,"duplicate":true}));
        }
    }
    let book_id = crate::agent_memory::context_book(db, s).await?;
    let note = json!({"id":id(),"date":date,"text":text,"sessionId":s["id"],
        "channel":s["channel"]["kind"].as_str().unwrap_or("desktop"),"bookId":book_id,
        "operationId":operation,"createdAt":now()});
    put(db, NS, &note_key(&date, note["id"].as_str().unwrap_or("")), &note).await?;
    Ok(json!({"id":note["id"],"date":date}))
}

pub async fn delete_note(db: &mut SqliteConnection, note_id: &str) -> Result<bool, String> {
    let Some(note) = list(db, NS, "journal:").await?.into_iter().find(|n| n["id"] == note_id) else {
        return Ok(false);
    };
    let key = note_key(note["date"].as_str().unwrap_or(""), note_id);
    Ok(delete_by_prefix(db, NS, &key).await? > 0)
}

/// 最近几天的备注，给模型的记忆上下文用（只带日期、内容、入口）
pub async fn recent_notes(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let since = local_date(now() - (CONTEXT_DAYS - 1) * 86_400_000);
    let mut notes = list(db, NS, "journal:").await?
        .into_iter()
        .filter(|n| n["date"].as_str().is_some_and(|d| d >= since.as_str()))
        .map(|n| json!({"date":n["date"],"text":n["text"],"channel":n["channel"]}))
        .collect::<Vec<_>>();
    notes.reverse();
    notes.truncate(CONTEXT_NOTES);
    notes.reverse();
    Ok(notes)
}

pub(crate) async fn stats(db: &mut SqliteConnection) -> Result<Value, String> {
    let Some(raw) = get(db, STATS_NS, STATS_KEY).await? else {
        return Ok(json!({}));
    };
    // 前端设置缓存里存的是 JSON 字符串
    Ok(match raw {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(json!({})),
        other => other,
    })
}

async fn book_title(db: &mut SqliteConnection, book_id: &str, cache: &mut BTreeMap<String, String>) -> Result<String, String> {
    if let Some(title) = cache.get(book_id) {
        return Ok(title.clone());
    }
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM local_books WHERE CAST(id AS TEXT)=?")
        .bind(book_id)
        .fetch_optional(&mut *db)
        .await
        .map_err(crate::agent_store::err)?;
    let title = title.unwrap_or_else(|| "已删除的作品".to_owned());
    cache.insert(book_id.to_owned(), title.clone());
    Ok(title)
}

fn session_book(s: &Value) -> Option<String> {
    [s["contextBookId"].as_str(), s["targetBookId"].as_str()]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
        .map(str::to_owned)
}

fn in_book(book: Option<&str>, value: &Value) -> bool {
    book.is_none_or(|b| value.as_str() == Some(b))
}

/// 某一天的工作：码字（按书）、对话、成果、章节改动、备注；book_id 可限定一本书
pub async fn day(db: &mut SqliteConnection, date: &str, book_id: Option<&str>) -> Result<Value, String> {
    let (start, end) = day_range(date)?;
    let mut titles = BTreeMap::new();
    let mut writing = Vec::new();
    if let Some(day) = stats(db).await?["days"][date].as_object() {
        for (bid, counts) in day {
            if !in_book(book_id, &json!(bid)) {
                continue;
            }
            let manual = counts["manual"].as_i64().unwrap_or(0);
            let ai = counts["ai"].as_i64().unwrap_or(0);
            if manual > 0 || ai > 0 {
                writing.push(json!({"bookId":bid,"title":book_title(db, bid, &mut titles).await?,"manual":manual,"ai":ai}));
            }
        }
    }
    let mut sessions = Vec::new();
    for s in list(db, NS, "session:").await? {
        if s["deletedAt"].is_number() || !in_book(book_id, &json!(session_book(&s))) {
            continue;
        }
        let (mut asked, mut replied) = (0, 0);
        for m in s["messages"].as_array().into_iter().flatten() {
            let at = m["createdAt"].as_i64().unwrap_or(0);
            if at < start || at >= end {
                continue;
            }
            if m["role"] == "user" {
                asked += 1;
            } else if m["role"] == "assistant" && m["text"].as_str().is_some_and(|t| !t.trim().is_empty()) {
                replied += 1;
            }
        }
        if asked > 0 || replied > 0 {
            sessions.push(json!({"sessionId":s["id"],"title":s["title"],"channel":s["channel"]["kind"].as_str().unwrap_or("desktop"),
                "bookId":session_book(&s),"userMessages":asked,"replies":replied,"legacy":s["legacy"]}));
        }
    }
    let mut artifacts = Vec::new();
    for a in list(db, NS, "artifact:").await? {
        let at = a["createdAt"].as_i64().unwrap_or(0);
        if at < start || at >= end || a["deletedAt"].is_number() || !in_book(book_id, &a["bookId"]) {
            continue;
        }
        artifacts.push(json!({"id":a["id"],"kind":a["kind"],"title":a["title"],"bookId":a["bookId"],"sessionId":a["sessionId"],"createdAt":at}));
    }
    artifacts.sort_by_key(|a| a["createdAt"].as_i64().unwrap_or(0));
    let rows = sqlx::query(
        "SELECT chapterId, json_extract(payload,'$.bookId') AS bookId, json_extract(payload,'$.title') AS title, json_extract(payload,'$.source') AS source \
         FROM chapter_versions WHERE createdAt>=? AND createdAt<? ORDER BY createdAt",
    )
    .bind(start)
    .bind(end)
    .fetch_all(&mut *db)
    .await
    .map_err(crate::agent_store::err)?;
    let mut chapters: BTreeMap<String, Value> = BTreeMap::new();
    for row in rows {
        let cid: i64 = row.try_get("chapterId").unwrap_or(0);
        let bid: String = row.try_get::<Option<String>, _>("bookId").ok().flatten().unwrap_or_default();
        let bid = bid.trim_matches('"').to_owned();
        if !in_book(book_id, &json!(bid)) {
            continue;
        }
        let title: String = row.try_get::<Option<String>, _>("title").ok().flatten().unwrap_or_default();
        let source: String = row.try_get::<Option<String>, _>("source").ok().flatten().unwrap_or_default();
        let entry = chapters.entry(cid.to_string()).or_insert_with(|| json!({"chapterId":cid.to_string(),"bookId":bid,"title":title,"versions":0,"remote":0}));
        entry["versions"] = json!(entry["versions"].as_i64().unwrap_or(0) + 1);
        if source == "remote" {
            entry["remote"] = json!(entry["remote"].as_i64().unwrap_or(0) + 1);
        }
    }
    let mut chapter_list = chapters.into_values().collect::<Vec<_>>();
    for c in &mut chapter_list {
        let bid = c["bookId"].as_str().unwrap_or("").to_owned();
        c["bookTitle"] = json!(if bid.is_empty() { String::new() } else { book_title(db, &bid, &mut titles).await? });
    }
    let notes = list(db, NS, &format!("journal:{date}:")).await?
        .into_iter()
        .filter(|n| in_book(book_id, &n["bookId"]) || n["bookId"].is_null())
        .collect::<Vec<_>>();
    Ok(json!({"date":date,"writing":writing,"sessions":sessions,"artifacts":artifacts,"chapters":chapter_list,"notes":notes}))
}

/// 有过活动的日期（最近在前）及各项计数，给工作备注页的日期列表用
pub async fn days(db: &mut SqliteConnection, limit: usize) -> Result<Vec<Value>, String> {
    let mut counts: BTreeMap<String, Value> = BTreeMap::new();
    let bump = |counts: &mut BTreeMap<String, Value>, date: &str, field: &str, by: i64| {
        let entry = counts.entry(date.to_owned()).or_insert_with(|| json!({"date":date,"words":0,"sessions":0,"artifacts":0,"chapters":0,"notes":0}));
        entry[field] = json!(entry[field].as_i64().unwrap_or(0) + by);
    };
    if let Some(days) = stats(db).await?["days"].as_object() {
        for (date, books) in days {
            let words = books.as_object().into_iter().flatten()
                .map(|(_, c)| c["manual"].as_i64().unwrap_or(0) + c["ai"].as_i64().unwrap_or(0))
                .sum::<i64>();
            if words > 0 {
                bump(&mut counts, date, "words", words);
            }
        }
    }
    for s in list(db, NS, "session:").await? {
        if s["deletedAt"].is_number() {
            continue;
        }
        let mut dates = std::collections::BTreeSet::new();
        for m in s["messages"].as_array().into_iter().flatten().filter(|m| m["role"] == "user") {
            if let Some(at) = m["createdAt"].as_i64() {
                dates.insert(local_date(at));
            }
        }
        for date in dates {
            bump(&mut counts, &date, "sessions", 1);
        }
    }
    for a in list(db, NS, "artifact:").await? {
        if let Some(at) = a["createdAt"].as_i64().filter(|_| !a["deletedAt"].is_number()) {
            bump(&mut counts, &local_date(at), "artifacts", 1);
        }
    }
    let stamps: Vec<(i64, i64)> = sqlx::query_as("SELECT createdAt, chapterId FROM chapter_versions")
        .fetch_all(&mut *db)
        .await
        .map_err(crate::agent_store::err)?;
    let mut touched = std::collections::BTreeSet::new();
    for (at, cid) in stamps {
        if touched.insert((local_date(at), cid)) {
            bump(&mut counts, &local_date(at), "chapters", 1);
        }
    }
    for n in list(db, NS, "journal:").await? {
        if let Some(date) = n["date"].as_str() {
            bump(&mut counts, date, "notes", 1);
        }
    }
    let mut out = counts.into_values().collect::<Vec<_>>();
    out.reverse();
    out.truncate(limit);
    Ok(out)
}
