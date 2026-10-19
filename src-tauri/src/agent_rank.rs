//! Agent 只读复用榜单风向的本地快照，不建立另一份榜单库，也不把缓存称为实时榜单。
use crate::agent_store::{err, get, list};
use chrono::NaiveDate;
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

pub const RANK_NS: &str = "ew-local-rank/kv";

fn date_arg(date: &str) -> Result<(), String> {
    if !date.is_empty()
        && (date.len() != 10 || NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err())
    {
        return Err("榜单日期应为 YYYY-MM-DD".into());
    }
    Ok(())
}

fn valid_head(doc: &Value) -> bool {
    doc["sourceId"].as_i64().is_some_and(|id| id > 0)
        && doc["statDate"]
            .as_str()
            .is_some_and(|date| !date.is_empty() && date_arg(date).is_ok())
}

fn newest_first(docs: &mut [Value]) {
    docs.sort_by(|a, b| b["statDate"].as_str().cmp(&a["statDate"].as_str()));
}

/// 单个榜单的快照：按键前缀读取，不解析其他榜单；格式不完整的快照跳过并计数，不拖垮整次查询。
async fn source_snapshots(
    db: &mut SqliteConnection,
    source_id: i64,
) -> Result<(Vec<Value>, usize), String> {
    let docs = list(db, RANK_NS, &format!("snap:{source_id}:")).await?;
    let total = docs.len();
    let mut docs = docs
        .into_iter()
        .filter(|doc| valid_head(doc) && doc["sourceId"] == source_id && doc["items"].is_array())
        .collect::<Vec<_>>();
    newest_first(&mut docs);
    let skipped = total - docs.len();
    Ok((docs, skipped))
}

/// 目录只需要元数据：字段在 SQLite 里取出，不把每份快照的全部条目读进内存。
async fn snapshot_catalog(db: &mut SqliteConnection) -> Result<(Vec<Value>, usize), String> {
    let prefix = crate::desktop_storage::key_prefix("snap:");
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('sourceId',json_extract(value,'$.sourceId'),\
         'statDate',json_extract(value,'$.statDate'),'fetchedAt',json_extract(value,'$.fetchedAt'),\
         'pageTitle',json_extract(value,'$.pageTitle'),'cutoffText',json_extract(value,'$.cutoffText'),\
         'itemCount',CASE json_type(value,'$.items') WHEN 'array' THEN json_array_length(value,'$.items') END) \
         FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=?",
    )
    .bind(RANK_NS)
    .bind(&prefix)
    .bind(&prefix)
    .fetch_all(db)
    .await
    .map_err(err)?;
    let total = rows.len();
    let mut docs = Vec::with_capacity(total);
    for raw in rows {
        let doc: Value = serde_json::from_str(&raw).map_err(err)?;
        if valid_head(&doc) && doc["itemCount"].is_u64() {
            docs.push(doc);
        }
    }
    newest_first(&mut docs);
    let skipped = total - docs.len();
    Ok((docs, skipped))
}

/// 可在对话里引用的榜单快照：最新的若干份（新到旧），只有元数据。
pub async fn recent_snapshots(db: &mut SqliteConnection, limit: usize) -> Result<Vec<Value>, String> {
    let (mut docs, _) = snapshot_catalog(db).await?;
    docs.truncate(limit);
    Ok(docs)
}

/// 引用里的榜单快照是否还在本地。
pub async fn snapshot_exists(db: &mut SqliteConnection, source_id: i64, date: &str) -> Result<bool, String> {
    date_arg(date)?;
    let (docs, _) = source_snapshots(db, source_id).await?;
    Ok(docs.iter().any(|d| d["statDate"] == date))
}

fn metadata(doc: &Value) -> Value {
    json!({"sourceId":doc["sourceId"],"statDate":doc["statDate"],"fetchedAt":doc["fetchedAt"],
        "pageTitle":doc["pageTitle"],"cutoffText":doc["cutoffText"],
        "itemCount":doc["items"].as_array().map_or_else(|| doc["itemCount"].clone(), |items| json!(items.len()))})
}

fn evidence_note() -> &'static str {
    "本地历史快照；本次查询没有检查网络数据。"
}

fn note_skipped(out: &mut Value, skipped: usize) {
    if skipped > 0 {
        out["skippedSnapshots"] = json!(skipped);
        out["skippedNote"] = json!("有快照格式不完整，已跳过；可在榜单风向重新抓取对应榜单");
    }
}

fn find_item<'a>(items: &'a [Value], title: &str) -> Option<&'a Value> {
    items.iter().find(|item| item["bookTitle"].as_str() == Some(title))
}

fn book_summary(item: &Value) -> Value {
    json!({"title":item["bookTitle"],"rankNo":item["rankNo"],"metricName":item["metricName"],
        "metricText":item["metricText"],"metricValue":item["metricValue"]})
}

/// 一本书在快照中的实际数据，展示文字由宿主生成，不经模型转述。
fn book_line(item: &Value) -> String {
    let mut parts = vec![format!("《{}》", item["bookTitle"].as_str().unwrap_or(""))];
    if let Some(rank) = item["rankNo"].as_i64() {
        parts.push(format!("第 {rank} 名"));
    }
    if let Some(metric) = item["metricText"].as_str().filter(|t| !t.is_empty()) {
        parts.push(format!("{} {metric}", item["metricName"].as_str().unwrap_or("指标")));
    }
    parts.join(" ")
}

/// 选题卡数据证据核对。模型只指明快照与书名，宿主从快照读出名次、指标并生成展示文字，
/// 所以“已核对”的内容全部来自快照，不依赖对模型句子的理解；模型的归纳写在判断证据里，不作核对。
/// 快照或书名对不上时降为待核实，不拒收整张卡。
pub async fn check_evidence(db: &mut SqliteConnection, evidence: &Value) -> Result<Value, String> {
    if evidence["kind"] != "data" {
        return Ok(json!({"status":"judgment"}));
    }
    let source_id = evidence["sourceId"].as_i64().unwrap_or(0);
    let date = evidence["snapshotDate"].as_str().unwrap_or("");
    let Some(doc) = get(db, RANK_NS, &format!("snap:{source_id}:{date}")).await? else {
        return Ok(json!({"status":"unverified","issue":format!("本地没有榜单 {source_id} 在 {date} 的快照")}));
    };
    let items = doc["items"].as_array().cloned().unwrap_or_default();
    let title = doc["pageTitle"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map_or_else(|| format!("榜单 {source_id}"), str::to_owned);
    let requested = evidence["books"].as_array().cloned().unwrap_or_default();
    let (mut found, mut missing) = (Vec::new(), Vec::new());
    for name in requested.iter().filter_map(Value::as_str) {
        match find_item(&items, name) {
            Some(item) => found.push(item),
            None => missing.push(format!("《{name}》")),
        }
    }
    let head = if requested.is_empty() {
        format!("{title}（{date} 快照，共 {} 本）", items.len())
    } else {
        format!("{title}（{date} 快照）")
    };
    let text = if found.is_empty() {
        head
    } else {
        format!("{head}：{}", found.iter().map(|item| book_line(item)).collect::<Vec<_>>().join("；"))
    };
    let snapshot = json!({"sourceId":source_id,"statDate":date,"pageTitle":doc["pageTitle"],
        "cutoffText":doc["cutoffText"],"fetchedAt":doc["fetchedAt"],"itemCount":items.len()});
    let books = found.iter().map(|item| book_summary(item)).collect::<Vec<_>>();
    if !missing.is_empty() {
        return Ok(json!({"status":"unverified","issue":format!("该快照中没有{}", missing.join("、")),
            "text":text,"snapshot":snapshot,"books":books}));
    }
    Ok(json!({"status":"verified","text":text,"snapshot":snapshot,"books":books}))
}

fn book_key(item: &Value) -> String {
    item["bookId"].as_str().filter(|s| !s.is_empty())
        .or_else(|| item["bookTitle"].as_str()).unwrap_or("").to_owned()
}

/// 与榜单页 attachCompare 一样按 bookId（缺失时书名）比较同一榜单的两份快照。
pub async fn trends(db: &mut SqliteConnection, source_id: i64, date: &str, compare_date: &str, limit: usize) -> Result<Value, String> {
    date_arg(date)?;
    date_arg(compare_date)?;
    let (docs, skipped) = source_snapshots(db, source_id).await?;
    let Some(current) = docs.iter().find(|d| date.is_empty() || d["statDate"] == date) else {
        return Ok(json!({"status":"not_found","dataSource":"local_snapshot","isLive":false,"note":evidence_note(),"message":"该榜单或日期没有本地快照"}));
    };
    let current_date = current["statDate"].as_str().unwrap();
    if !compare_date.is_empty() && compare_date >= current_date {
        return Err("对比日期必须早于当前快照日期".into());
    }
    let previous = docs.iter().find(|d| {
        let date = d["statDate"].as_str().unwrap();
        date < current_date && (compare_date.is_empty() || date == compare_date)
    });
    let Some(previous) = previous else {
        return Ok(json!({"status":"insufficient_history","dataSource":"local_snapshot","isLive":false,
            "snapshot":metadata(current),"note":evidence_note(),"message":"缺少可比较的历史快照，不能判断排名上升、下降或题材趋势"}));
    };
    let prior = previous["items"].as_array().unwrap().iter().map(|i| (book_key(i), i)).collect::<BTreeMap<_,_>>();
    let mut items = current["items"].as_array().unwrap().clone();
    items.sort_by_key(|item| item["rankNo"].as_i64().unwrap_or(i64::MAX));
    let total = items.len();
    items.truncate(limit.clamp(1, 100));
    for item in &mut items {
        let before = prior.get(&book_key(item));
        let old_rank = before.and_then(|v| v["rankNo"].as_i64());
        let change = old_rank.zip(item["rankNo"].as_i64()).map(|(a,b)| a-b);
        let metric_delta = before.filter(|v| v["metricName"] == item["metricName"])
            .and_then(|v| v["metricValue"].as_f64()).zip(item["metricValue"].as_f64()).map(|(a,b)| b-a);
        item["prevRankNo"] = json!(old_rank);
        item["rankChangeDelta"] = json!(change);
        item["metricDelta"] = json!(metric_delta);
        item["newInSnapshot"] = json!(before.is_none());
    }
    let mut out = json!({"status":"available","dataSource":"local_snapshot","isLive":false,"note":evidence_note(),
        "snapshot":metadata(current),"compareSnapshot":metadata(previous),"items":items,"truncated":total>limit.clamp(1,100)});
    note_skipped(&mut out, skipped);
    Ok(out)
}
