//! 作品管理：列作品、把书带进对话、改卷章与作品信息、软删除进回收站。
//! 与书架同一套表（local_books / local_volumes / local_chapters）、同一种软删除（deletedAt 列与 payload 同步），
//! 统计字段按未删除章节重算；删除前已由审批门经作者确认，这里不再问。
use crate::agent_store::*;
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::path::Path;

fn stamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn id_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn clean(value: &Value) -> String {
    value.as_str().unwrap_or("").trim().to_owned()
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

fn tool_error(code: &str, message: &str, retryable: bool) -> Value {
    json!({"ok":false,"toolError":{"code":code,"retryable":retryable,"message":message}})
}

fn valid_title(title: &str) -> Result<(), Value> {
    if title.chars().count() > 120 {
        return Err(tool_error("invalid_arguments", "标题应为 1—120 字", true));
    }
    Ok(())
}

/// 书架上的作品（未删除，最近编辑在前），可按书名或分类筛
pub async fn list_books(db: &mut SqliteConnection, keyword: &str, limit: usize) -> Result<Value, String> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT payload FROM local_books WHERE deletedAt IS NULL ORDER BY updateTime DESC")
        .fetch_all(db)
        .await
        .map_err(err)?;
    let keyword = keyword.trim();
    let mut books = Vec::new();
    let mut total = 0usize;
    for row in rows {
        let b: Value = serde_json::from_str(&row).map_err(err)?;
        let title = b["title"].as_str().unwrap_or("");
        if !keyword.is_empty() && !title.contains(keyword) && !b["category"].as_str().unwrap_or("").contains(keyword) {
            continue;
        }
        total += 1;
        if books.len() < limit.max(1) {
            books.push(json!({
                "id": id_text(&b["id"]),
                "title": title,
                "category": b["category"].as_str().unwrap_or(""),
                "tags": b["tags"].as_array().cloned().unwrap_or_default(),
                "intro": clip(b["intro"].as_str().unwrap_or(""), 80),
                "wordCount": b["wordCount"].as_i64().unwrap_or(0),
                "chapterCount": b["chapterCount"].as_i64().unwrap_or(0),
                "updateTime": b["updateTime"].as_str().unwrap_or(""),
            }));
        }
    }
    Ok(json!({"books":books,"total":total}))
}

/// 把一本书带进对话：加入引用并作为上下文作品。只是打开，不授予写权限。
pub async fn open_book(root: &Path, sid: &str, bid: &str) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let b = book(&mut tx, bid).await?;
    let mut s = session(&mut tx, sid).await?;
    let title = b["title"].as_str().unwrap_or("").to_owned();
    let mut refs = s["references"].as_array().cloned().unwrap_or_default();
    let already = refs.iter().any(|r| r["kind"] == "book" && r["bookId"] == bid);
    if !already {
        refs.push(json!({"id":format!("book:{bid}"),"kind":"book","bookId":bid,"title":title,"description":""}));
        s["references"] = json!(refs);
    }
    s["contextBookId"] = json!(bid);
    save_session(&mut tx, &mut s).await?;
    tx.commit().await.map_err(err)?;
    Ok(json!({"opened":true,"alreadyOpen":already,"book":{
        "id":bid,"title":title,"category":b["category"].as_str().unwrap_or(""),
        "chapterCount":b["chapterCount"].as_i64().unwrap_or(0),"wordCount":b["wordCount"].as_i64().unwrap_or(0)}}))
}

/// 同一操作编号只执行一次：再次调用返回当时的结果，参数不同则拒绝
async fn receipt(db: &mut SqliteConnection, sid: &str, tool: &str, args: &Value) -> Result<(String, Option<Value>), String> {
    let op = str_field(args, "operation_id")?;
    let key = format!("library:{sid}:{op}");
    if let Some(old) = get(db, OP_NS, &key).await? {
        if old["tool"] != tool || old["request"] != *args {
            return Err("重复操作编号对应不同请求".into());
        }
        return Ok((key, Some(old["result"].clone())));
    }
    Ok((key, None))
}

async fn remember(db: &mut SqliteConnection, key: &str, tool: &str, args: &Value, result: &Value) -> Result<(), String> {
    put(db, OP_NS, key, &json!({"tool":tool,"request":args,"result":result})).await
}

/// 书的统计按未删除章节重算，与正文保存走同一组字段；前端的字数缓存一并失效
pub async fn refresh_book_stats(db: &mut SqliteConnection, bid: &str) -> Result<(), String> {
    let now = stamp();
    sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.wordCount',COALESCE((SELECT SUM(COALESCE(json_extract(payload,'$.wordCount'),0)) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),0),'$.textWordCount',COALESCE((SELECT SUM(COALESCE(json_extract(payload,'$.textWordCount'),0)) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),0),'$.chapterCount',(SELECT COUNT(*) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(bid).bind(bid).bind(bid).bind(&now).bind(&now).bind(bid)
        .execute(&mut *db).await.map_err(err)?;
    sqlx::query("DELETE FROM sync_settings WHERE key LIKE 'bookWordCounts:%'")
        .execute(&mut *db)
        .await
        .map_err(err)?;
    Ok(())
}

async fn touch_book(db: &mut SqliteConnection, bid: &str) -> Result<(), String> {
    let now = stamp();
    sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(&now).bind(&now).bind(bid)
        .execute(db).await.map_err(err)?;
    Ok(())
}

async fn entry(db: &mut SqliteConnection, table: &str, bid: &str, eid: &str) -> Result<Option<Value>, String> {
    let row: Option<String> = sqlx::query_scalar(&format!("SELECT payload FROM {table} WHERE CAST(id AS TEXT)=? AND bookId=? AND deletedAt IS NULL"))
        .bind(eid).bind(bid)
        .fetch_optional(db).await.map_err(err)?;
    row.map(|r| serde_json::from_str(&r).map_err(err)).transpose()
}

/// 改一卷或一章的标题、简介（章纲）；不动正文
pub async fn update_entry(root: &Path, sid: &str, kind: &str, tool: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    crate::agent_service::allowed_book(&mut tx, &s, bid, true).await?;
    let (key, done) = receipt(&mut tx, sid, tool, args).await?;
    if let Some(result) = done {
        return Ok(result);
    }
    let (table, id_field, noun) = if kind == "volume" {
        ("local_volumes", "volume_id", ("卷名", "简介"))
    } else {
        ("local_chapters", "chapter_id", ("章名", "章纲"))
    };
    let eid = str_field(args, id_field)?;
    let title = clean(&args["title"]);
    let summary = clean(&args["summary"]);
    if title.is_empty() && summary.is_empty() {
        return Ok(tool_error("invalid_arguments", &format!("没有要改的内容：{}和{}至少改一项", noun.0, noun.1), true));
    }
    if let Err(error) = valid_title(&title) {
        return Ok(error);
    }
    let Some(mut item) = entry(&mut tx, table, bid, eid).await? else {
        return Ok(tool_error(
            &format!("{kind}_not_found"),
            &format!("这{}已不存在或已在回收站，没有修改。", if kind == "volume" { "一卷" } else { "一章" }),
            true,
        ));
    };
    let before = item["title"].as_str().unwrap_or("").to_owned();
    let mut changes = Vec::new();
    if !title.is_empty() {
        item["title"] = json!(title);
        changes.push(format!("{}改为《{}》", noun.0, title));
    }
    if !summary.is_empty() {
        item["summary"] = json!(summary);
        changes.push(format!("{}改为「{}」", noun.1, clip(&summary, 40)));
    }
    item["updateTime"] = json!(stamp());
    sqlx::query(&format!("UPDATE {table} SET payload=? WHERE CAST(id AS TEXT)=? AND bookId=?"))
        .bind(item.to_string()).bind(eid).bind(bid)
        .execute(&mut *tx).await.map_err(err)?;
    touch_book(&mut tx, bid).await?;
    let result = json!({"updated":true,"bookId":bid,"kind":kind,
        "entry":{"id":eid,"title":item["title"],"summary":item["summary"]},
        "before":{"title":before},"changes":changes});
    remember(&mut tx, &key, tool, args, &result).await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}

/// 改书名、简介、分类、标签；标签整体替换
pub async fn update_book(root: &Path, sid: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    crate::agent_service::allowed_book(&mut tx, &s, bid, true).await?;
    let (key, done) = receipt(&mut tx, sid, "update_book", args).await?;
    if let Some(result) = done {
        return Ok(result);
    }
    let mut b = book(&mut tx, bid).await?;
    let before = b["title"].as_str().unwrap_or("").to_owned();
    let mut changes = Vec::new();
    let title = clean(&args["title"]);
    if let Err(error) = valid_title(&title) {
        return Ok(error);
    }
    if !title.is_empty() && title != before {
        b["title"] = json!(title);
        changes.push(format!("书名改为《{title}》"));
    }
    let intro = clean(&args["intro"]);
    if !intro.is_empty() {
        b["intro"] = json!(intro);
        changes.push("简介已更新".to_owned());
    }
    let category = clean(&args["category"]);
    if !category.is_empty() {
        b["category"] = json!(category);
        changes.push(format!("分类改为 {category}"));
    }
    if let Some(tags) = args["tags"].as_array() {
        let tags: Vec<String> = tags.iter().filter_map(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).collect();
        if !tags.is_empty() {
            changes.push(format!("标签改为 {}", tags.join("/")));
            b["tags"] = json!(tags);
        }
    }
    if changes.is_empty() {
        return Ok(tool_error("invalid_arguments", "没有要改的内容：书名、简介、分类、标签至少改一项，且要和现在不同", true));
    }
    let now = stamp();
    b["updateTime"] = json!(now);
    sqlx::query("UPDATE local_books SET payload=?, title=?, updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(b.to_string()).bind(b["title"].as_str().unwrap_or("")).bind(&now).bind(bid)
        .execute(&mut *tx).await.map_err(err)?;
    let result = json!({"updated":true,"bookId":bid,
        "book":{"id":bid,"title":b["title"],"category":b["category"],"tags":b["tags"],"intro":b["intro"]},
        "before":{"title":before},"changes":changes});
    remember(&mut tx, &key, "update_book", args, &result).await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}

/// 删一章或一卷（连同卷内章节）：软删除进回收站，书的统计重算
pub async fn delete_entry(root: &Path, sid: &str, kind: &str, tool: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    // 删除的授权来自作者点选的确认卡；这里只要求这本书在对话范围内
    crate::agent_service::allowed_book(&mut tx, &s, bid, false).await?;
    let (key, done) = receipt(&mut tx, sid, tool, args).await?;
    if let Some(result) = done {
        return Ok(result);
    }
    let (table, id_field) = if kind == "volume" { ("local_volumes", "volume_id") } else { ("local_chapters", "chapter_id") };
    let eid = str_field(args, id_field)?;
    let Some(item) = entry(&mut tx, table, bid, eid).await? else {
        return Ok(tool_error(
            &format!("{kind}_not_found"),
            &format!("这{}已不存在或已在回收站，没有删除。", if kind == "volume" { "一卷" } else { "一章" }),
            true,
        ));
    };
    let now = stamp();
    sqlx::query(&format!("UPDATE {table} SET deletedAt=?, payload=json_set(payload,'$.deletedAt',?,'$.updateTime',?) WHERE CAST(id AS TEXT)=? AND bookId=? AND deletedAt IS NULL"))
        .bind(&now).bind(&now).bind(&now).bind(eid).bind(bid)
        .execute(&mut *tx).await.map_err(err)?;
    let mut chapters = 0u64;
    if kind == "volume" {
        chapters = sqlx::query("UPDATE local_chapters SET deletedAt=?, payload=json_set(payload,'$.deletedAt',?,'$.updateTime',?) WHERE volumeId=? AND bookId=? AND deletedAt IS NULL")
            .bind(&now).bind(&now).bind(&now).bind(eid).bind(bid)
            .execute(&mut *tx).await.map_err(err)?.rows_affected();
    }
    refresh_book_stats(&mut tx, bid).await?;
    let result = json!({"deleted":true,"recycleBin":true,"bookId":bid,"kind":kind,
        "entry":{"id":eid,"title":item["title"],"wordCount":item["wordCount"].as_i64().unwrap_or(0)},
        "chapters": if kind == "volume" { json!(chapters) } else { Value::Null }});
    remember(&mut tx, &key, tool, args, &result).await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}

/// 整本书进回收站；会话里不再引用它，也不再把它当上下文或写入目标
pub async fn delete_book(root: &Path, sid: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let mut s = session(&mut tx, sid).await?;
    crate::agent_service::allowed_book(&mut tx, &s, bid, false).await?;
    let (key, done) = receipt(&mut tx, sid, "delete_book", args).await?;
    if let Some(result) = done {
        return Ok(result);
    }
    let b = book(&mut tx, bid).await?;
    let now = stamp();
    sqlx::query("UPDATE local_books SET deletedAt=?, updateTime=?, payload=json_set(payload,'$.deletedAt',?,'$.updateTime',?) WHERE CAST(id AS TEXT)=? AND deletedAt IS NULL")
        .bind(&now).bind(&now).bind(&now).bind(&now).bind(bid)
        .execute(&mut *tx).await.map_err(err)?;
    if let Some(refs) = s["references"].as_array() {
        let kept: Vec<Value> = refs.iter().filter(|r| !(r["kind"] != "material" && r["bookId"] == bid)).cloned().collect();
        s["references"] = json!(kept);
    }
    if s["targetBookId"] == bid {
        s["targetBookId"] = Value::Null;
    }
    if s["contextBookId"] == bid {
        s["contextBookId"] = Value::Null;
    }
    save_session(&mut tx, &mut s).await?;
    let result = json!({"deleted":true,"recycleBin":true,"bookId":bid,
        "book":{"id":bid,"title":b["title"],"chapterCount":b["chapterCount"].as_i64().unwrap_or(0)}});
    remember(&mut tx, &key, "delete_book", args, &result).await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}
