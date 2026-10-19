//! 兼容旧版本已保存的规则汇编及作者确认操作。
//! 新版上下文装配不创建汇编，也不因作者尚未确认旧汇编而拒绝写作。
use crate::agent_canon as canon;
use crate::agent_store::{get, now, put, str_field, NS};
#[cfg(test)]
use crate::agent_store::list;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 被包含的条目至少这么长才算“完全包含”，免得“短”这类单字被长句吞掉。
#[cfg(test)]
const MIN_SUBSUMED_CHARS: usize = 4;

fn digest_key(book_id: &str) -> String {
    format!("memory:digest-{book_id}")
}

/// 比较用的文字：去掉空白与标点。
#[cfg(test)]
fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

/// 在 items（编号, 文字）里找出可删的：与前面某条相同，或被另一条更长的完全包含。返回（可删编号, 保留的那条文字）。
#[cfg(test)]
fn redundant(items: &[(String, String)]) -> Vec<(String, String)> {
    let norms: Vec<String> = items.iter().map(|(_, t)| normalized(t)).collect();
    let mut out = Vec::new();
    for (i, (id, _)) in items.iter().enumerate() {
        let n = &norms[i];
        if n.is_empty() {
            continue;
        }
        let keeper = items.iter().enumerate().find(|(j, _)| {
            let m = &norms[*j];
            *j != i
                && !out.iter().any(|(removed, _): &(String, String)| removed == &items[*j].0)
                && ((m == n && *j < i) || (m.len() > n.len() && n.chars().count() >= MIN_SUBSUMED_CHARS && m.contains(n.as_str())))
        });
        if let Some((_, (_, kept))) = keeper {
            out.push((id.clone(), kept.clone()));
        }
    }
    out
}

/// 仅用于生成旧版数据夹具，验证历史汇编仍可由作者确认或拒绝。
#[cfg(test)]
pub async fn propose(db: &mut SqliteConnection, book_id: &str) -> Result<Option<Value>, String> {
    // 作品规则排在全局规则后面：同样的话两处都有时删作品那条，全局的照样生效
    let mut rules = canon::list_rules(db, Some(book_id)).await?;
    rules.sort_by_key(|r| (r["scope"] != "author", r["createdAt"].as_i64().unwrap_or(0)));
    let rule_items: Vec<(String, String)> = rules
        .iter()
        .map(|r| (r["id"].as_str().unwrap_or("").to_owned(), format!("[{}] {}", r["category"].as_str().unwrap_or(""), r["text"].as_str().unwrap_or(""))))
        .collect();
    let rule_texts: Vec<(String, String)> = rules
        .iter()
        .map(|r| (r["id"].as_str().unwrap_or("").to_owned(), r["text"].as_str().unwrap_or("").to_owned()))
        .collect();
    let memories = list(db, NS, "memory:")
        .await?
        .into_iter()
        .filter(|m| m["bookId"] == book_id && m["status"] == "rejected" && !m["deletedAt"].is_number())
        .map(|m| (m["id"].as_str().unwrap_or("").to_owned(), m["content"].as_str().unwrap_or("").to_owned()))
        .collect::<Vec<_>>();
    let removed_rules = redundant(&rule_texts);
    let removed_memories = redundant(&memories);
    if removed_rules.is_empty() && removed_memories.is_empty() {
        return Ok(None);
    }
    let label = |items: &[(String, String)], id: &str| items.iter().find(|(i, _)| i == id).map(|(_, t)| t.clone()).unwrap_or_default();
    let mut lines = Vec::new();
    for (id, kept) in &removed_rules {
        lines.push(format!("删去规则 {}（与“{}”重复或被它包含）", label(&rule_items, id), kept));
    }
    for (id, kept) in &removed_memories {
        lines.push(format!("删去否决约束“{}”（与“{}”重复或被它包含）", label(&memories, id), kept));
    }
    let saved_chars: usize = removed_rules.iter().map(|(id, _)| label(&rule_items, id).chars().count()).sum::<usize>()
        + removed_memories.iter().map(|(id, _)| label(&memories, id).chars().count()).sum::<usize>();
    let plan = json!({"rules":removed_rules.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        "memories":removed_memories.iter().map(|(id, _)| id).collect::<Vec<_>>()});
    let key = digest_key(book_id);
    if let Some(existing) = get(db, NS, &key).await? {
        let same = existing["digest"]["plan"] == plan;
        if same && (existing["status"] == "pending" || existing["status"] == "rejected" || existing["deletedAt"].is_number()) {
            return Ok((existing["status"] == "pending" && !existing["deletedAt"].is_number()).then_some(existing));
        }
    }
    let record = json!({"id":format!("digest-{book_id}"),"bookId":book_id,"category":"规则汇编","kind":"candidate","scope":"book","status":"pending",
        "content":format!("可以合并 {} 条重复或被包含的规则与否决约束，约省 {} 字", lines.len(), saved_chars),
        "quote":lines.join("\n"),"source":"作者规则整理","period":"当前作品",
        "digest":{"plan":plan,"savedChars":saved_chars},"revision":0,"createdAt":now(),"updatedAt":now()});
    put(db, NS, &key, &record).await?;
    Ok(Some(record))
}

/// 作者确认汇编：删去其中列出的规则与否决约束，汇编本身归档，不进入写作上下文。
pub async fn apply(db: &mut SqliteConnection, book_id: &str) -> Result<Value, String> {
    let key = digest_key(book_id);
    let mut record = get(db, NS, &key).await?.ok_or("没有待确认的规则汇编")?;
    if record["status"] != "pending" || record["deletedAt"].is_number() {
        return Err("这条规则汇编已处理过".into());
    }
    let mut removed = 0;
    for rule in record["digest"]["plan"]["rules"].as_array().cloned().unwrap_or_default() {
        if let Some(rule_key) = rule.as_str() {
            if get(db, canon::AUTHOR_NS, rule_key).await?.is_some_and(|r| !r["deletedAt"].is_number()) {
                canon::delete_rule(db, rule_key).await?;
                removed += 1;
            }
        }
    }
    for memory in record["digest"]["plan"]["memories"].as_array().cloned().unwrap_or_default() {
        let memory_key = format!("memory:{}", memory.as_str().unwrap_or(""));
        if let Some(mut m) = get(db, NS, &memory_key).await? {
            if !m["deletedAt"].is_number() {
                m["deletedAt"] = json!(now());
                m["updatedAt"] = json!(now());
                crate::agent_memory::store(db, &m).await?;
                removed += 1;
            }
        }
    }
    record["status"] = json!("archived");
    record["appliedAt"] = json!(now());
    record["updatedAt"] = json!(now());
    put(db, NS, &key, &record).await?;
    Ok(json!({"removed":removed,"memory":record,"bookId":str_field(&record, "bookId")?}))
}

#[cfg(test)]
pub(crate) fn redundant_for_test(items: &[(String, String)]) -> Vec<(String, String)> {
    redundant(items)
}
