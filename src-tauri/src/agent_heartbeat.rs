//! 事件提醒里的周期检查：每小时看一眼数据表，有事才提醒、没事静默，不调模型。
//! 每条检查对应"事件提醒"里的一条规则（能开关、改阈值、选送达方式）：伏笔久未推进、今日码字达标、
//! 榜单快照过期、停笔后整理记忆。每条告警带键，同一件事一天只说一次。拆书与批量结束走事件订阅，不在这里。
use crate::agent_store::*;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 最近几天有正文改动的书才巡检伏笔
const ACTIVE_BOOK_DAYS: i64 = 7;
/// 停笔整理：超过起点多久就不再算"刚停笔"（分钟）
const IDLE_WINDOW_MINUTES: i64 = 240;
/// 同一告警键再次提醒的最短间隔
const REPEAT_MS: i64 = 24 * 60 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Alert {
    pub event: &'static str,
    pub key: String,
    pub text: String,
}

pub struct Report {
    pub alerts: Vec<Alert>,
    /// 要宿主做的事：index_now
    pub actions: Vec<&'static str>,
    /// 写回设置记录的检查状态（已提醒过的键、已整理过的停笔点、上次检查时间）
    pub state: Value,
}

/// 是否在提醒时段（本地小时，[start, end)）；settings["activeHours"] 缺省 8–23
pub fn within_active_hours(settings: &Value, local_hour: u32) -> bool {
    let start = settings["activeHours"][0].as_u64().unwrap_or(8) as u32;
    let end = settings["activeHours"][1].as_u64().unwrap_or(23) as u32;
    if start <= end { (start..end).contains(&local_hour) } else { local_hour >= start || local_hour < end }
}

fn rule<'a>(rules: &'a [Value], event: &str) -> Option<&'a Value> {
    rules.iter().find(|r| r["event"] == event && r["enabled"] == true)
}

fn param(rule: &Value, key: &str, default: i64) -> i64 {
    rule["params"][key].as_i64().filter(|v| *v > 0).unwrap_or(default)
}

/// 跑一遍所有启用的检查。`state` 是上次留下的检查状态。
pub async fn run(db: &mut SqliteConnection, rules: &[Value], state: &Value, now_ms: i64) -> Result<Report, String> {
    let mut state = if state.is_object() { state.clone() } else { json!({}) };
    let mut alerts = Vec::new();
    let mut actions = Vec::new();
    if let Some(r) = rule(rules, "threads_overdue") {
        alerts.extend(overdue_threads(db, now_ms, param(r, "chapters", 15) as usize).await?);
    }
    if rule(rules, "words_target_reached").is_some() {
        alerts.extend(words_target(db).await?);
    }
    if let Some(r) = rule(rules, "rank_stale") {
        alerts.extend(stale_ranks(db, param(r, "days", 3)).await?);
    }
    if let Some(r) = rule(rules, "idle_consolidate") {
        if let Some(stamp) = idle_point(db, now_ms, param(r, "minutes", 30)).await? {
            if state["consolidatedFor"].as_i64() != Some(stamp) {
                state["consolidatedFor"] = json!(stamp);
                actions.push("index_now");
            }
        }
    }
    // 去重：同键 24 小时内只提醒一次；状态里只留还在窗口内的键
    let mut notified: serde_json::Map<String, Value> = state["notified"].as_object().cloned().unwrap_or_default();
    notified.retain(|_, at| now_ms - at.as_i64().unwrap_or(0) < REPEAT_MS);
    let mut fresh = Vec::new();
    for alert in alerts {
        if notified.contains_key(&alert.key) {
            continue;
        }
        notified.insert(alert.key.clone(), json!(now_ms));
        fresh.push(alert);
    }
    state["notified"] = Value::Object(notified);
    state["checkedAt"] = json!(now_ms);
    Ok(Report { alerts: fresh, actions, state })
}

/// 最近有正文改动的作品编号
async fn active_books(db: &mut SqliteConnection, now_ms: i64) -> Result<Vec<String>, String> {
    sqlx::query_scalar::<_, String>("SELECT DISTINCT bookId FROM chapter_contents WHERE updatedAt>=? ORDER BY bookId")
        .bind(now_ms - ACTIVE_BOOK_DAYS * DAY_MS)
        .fetch_all(db)
        .await
        .map_err(err)
}

fn position(catalog: &[Value], chapter_id: &Value) -> Option<usize> {
    let wanted = match chapter_id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    catalog.iter().position(|e| match &e["id"] {
        Value::String(s) => *s == wanted,
        Value::Number(n) => n.to_string() == wanted,
        _ => false,
    })
}

/// 伏笔到期：埋下后超过 N 章没推进也没回收
async fn overdue_threads(db: &mut SqliteConnection, now_ms: i64, overdue_chapters: usize) -> Result<Vec<Alert>, String> {
    let mut out = Vec::new();
    for bid in active_books(db, now_ms).await? {
        let Ok(entries) = catalog(db, &bid).await else { continue };
        let Some(latest) = entries.iter().rposition(|e| e["wordCount"].as_i64().unwrap_or(0) > 0) else { continue };
        let mut overdue = Vec::new();
        for thread in crate::agent_canon::list_threads(db, &bid, Some("open")).await? {
            let planted = position(&entries, &thread["plantedChapter"]);
            let last_touch = thread["advances"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| position(&entries, &a["chapterId"]))
                .chain(planted)
                .max();
            let (Some(planted), Some(last_touch)) = (planted, last_touch) else { continue };
            if latest.saturating_sub(last_touch) >= overdue_chapters {
                overdue.push((planted + 1, thread["title"].as_str().unwrap_or("").to_owned(), thread["threadId"].as_str().unwrap_or("").to_owned()));
            }
        }
        if overdue.is_empty() {
            continue;
        }
        overdue.sort();
        let title = book(db, &bid).await.ok().and_then(|b| b["title"].as_str().map(str::to_owned)).unwrap_or_else(|| "这本书".into());
        let listed = overdue.iter().take(3).map(|(ch, t, _)| format!("「{t}」（第{ch}章埋下）")).collect::<Vec<_>>().join("、");
        let more = if overdue.len() > 3 { format!(" 等 {} 条", overdue.len()) } else { String::new() };
        out.push(Alert {
            event: "threads_overdue",
            key: format!("threads:{bid}:{}", overdue.iter().map(|(_, _, id)| id.as_str()).collect::<Vec<_>>().join(",")),
            text: format!("《{title}》有伏笔超过 {overdue_chapters} 章没推进：{listed}{more}。"),
        });
    }
    Ok(out)
}

/// 今日字数达标：手写加 AI 的净增达到目标，一天只说一次
async fn words_target(db: &mut SqliteConnection) -> Result<Vec<Alert>, String> {
    let stats = crate::agent_journal::stats(db).await?;
    let target = stats["targets"]["manual"].as_i64().unwrap_or(0) + stats["targets"]["ai"].as_i64().unwrap_or(0);
    if target <= 0 {
        return Ok(vec![]);
    }
    let date = crate::agent_journal::today();
    let total: i64 = stats["days"][&date]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, c)| c["manual"].as_i64().unwrap_or(0) + c["ai"].as_i64().unwrap_or(0))
        .sum();
    if total >= target {
        return Ok(vec![Alert { event: "words_target_reached", key: format!("words:{date}"), text: format!("今天码字 {total} 字，已达到目标 {target} 字。") }]);
    }
    Ok(vec![])
}

/// 榜单快照过期：每个榜单最新一份的日期距今超过 N 天
async fn stale_ranks(db: &mut SqliteConnection, stale_days: i64) -> Result<Vec<Alert>, String> {
    let today = chrono::Local::now().date_naive();
    let mut latest: std::collections::BTreeMap<i64, (String, String)> = std::collections::BTreeMap::new();
    for doc in crate::agent_rank::recent_snapshots(db, 60).await? {
        let source = doc["sourceId"].as_i64().unwrap_or(0);
        let date = doc["statDate"].as_str().unwrap_or("").to_owned();
        let title = doc["pageTitle"].as_str().unwrap_or("").to_owned();
        let entry = latest.entry(source).or_insert((date.clone(), title.clone()));
        if date > entry.0 {
            *entry = (date, title);
        }
    }
    // 过期的榜单合成一条提醒：几十个榜单各发一条会刷屏（2026-10-15 一次发了 35 条）
    let mut stale: Vec<(i64, String, String, i64)> = Vec::new();
    for (source, (date, title)) in latest {
        let Ok(stat) = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") else { continue };
        let days = (today - stat).num_days();
        if days >= stale_days {
            stale.push((source, date, title, days));
        }
    }
    if stale.is_empty() {
        return Ok(Vec::new());
    }
    stale.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let (source, date, title, days) = &stale[0];
    let name = if title.is_empty() { format!("榜单 {source}") } else { format!("榜单「{title}」") };
    let text = if stale.len() == 1 {
        format!("{name}的本地快照还是 {date} 的，已 {days} 天没更新；要看最新风向可以让我刷新。")
    } else {
        format!("{} 个榜单的本地快照超过 {stale_days} 天没更新，最旧的是 {date}（{name}）；要看最新风向可以让我刷新。", stale.len())
    };
    // 键只看最旧那份的日期：同一批过期一天只提醒一次，刷新后日期变了才再提
    Ok(vec![Alert { event: "rank_stale", key: format!("rank_stale:{date}"), text }])
}

/// 停笔点：最近一次正文改动在 N 分钟到 N+4 小时之前，返回那次改动的时间作为整理标记
async fn idle_point(db: &mut SqliteConnection, now_ms: i64, idle_minutes: i64) -> Result<Option<i64>, String> {
    let last: Option<i64> = sqlx::query_scalar("SELECT MAX(updatedAt) FROM chapter_contents")
        .fetch_one(db)
        .await
        .map_err(err)?;
    let Some(last) = last else { return Ok(None) };
    let idle = (now_ms - last) / 60_000;
    Ok((idle >= idle_minutes && idle <= idle_minutes + IDLE_WINDOW_MINUTES).then_some(last))
}
