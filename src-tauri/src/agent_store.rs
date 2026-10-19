//! Agent 的业务记录存放在平台已有 desktop_records 中，正式正文不复制到项目库。
//! 会话只由宿主写入；前端提交具体命令，不能用旧会话快照覆盖正在运行的任务。
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};
#[cfg(test)]
use std::path::Path;

pub const NS: &str = "ew-agent/records";
pub const CHECKPOINT_NS: &str = "ew-agent-checkpoints/records";
pub const OP_NS: &str = "ew-agent-operations/records";
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub fn str_field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("缺少 {key}"))
}
pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub async fn get(db: &mut SqliteConnection, ns: &str, key: &str) -> Result<Option<Value>, String> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM desktop_records WHERE namespace=? AND key=?")
            .bind(ns)
            .bind(json!(key).to_string())
            .fetch_optional(db)
            .await
            .map_err(err)?;
    raw.map(|s| serde_json::from_str(&s).map_err(err))
        .transpose()
}
pub async fn put(
    db: &mut SqliteConnection,
    ns: &str,
    key: &str,
    value: &Value,
) -> Result<(), String> {
    sqlx::query("INSERT INTO desktop_records(namespace,key,value) VALUES(?,?,?) ON CONFLICT(namespace,key) DO UPDATE SET value=excluded.value")
        .bind(ns).bind(json!(key).to_string()).bind(value.to_string()).execute(db).await.map_err(err)?;
    Ok(())
}
pub async fn list(db: &mut SqliteConnection, ns: &str, prefix: &str) -> Result<Vec<Value>, String> {
    // 前缀在 SQL 里过滤：命名空间里其他大记录（如其他榜单的快照）不读出、不解析。
    let encoded = crate::desktop_storage::key_prefix(prefix);
    let rows = sqlx::query(
        "SELECT key,value FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=? ORDER BY key",
    )
    .bind(ns)
    .bind(&encoded)
    .bind(&encoded)
    .fetch_all(db)
    .await
    .map_err(err)?;
    rows.into_iter()
        .filter_map(|r| {
            let key = serde_json::from_str::<String>(r.get("key"));
            match key {
                Ok(key) if key.starts_with(prefix) => {
                    Some(serde_json::from_str(r.get("value")).map_err(err))
                }
                Ok(_) => None,
                Err(e) => Some(Err(err(e))),
            }
        })
        .collect()
}
/// 按键前缀删一批记录（遗忘会话时清检查点与短期回执用）；返回删掉的条数
pub async fn delete_by_prefix(db: &mut SqliteConnection, ns: &str, prefix: &str) -> Result<u64, String> {
    let encoded = crate::desktop_storage::key_prefix(prefix);
    Ok(sqlx::query("DELETE FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=?")
        .bind(ns)
        .bind(&encoded)
        .bind(&encoded)
        .execute(db)
        .await
        .map_err(err)?
        .rows_affected())
}

pub async fn session(db: &mut SqliteConnection, session_id: &str) -> Result<Value, String> {
    let s = get(db, NS, &format!("session:{session_id}"))
        .await?
        .ok_or("会话不存在")?;
    if s["deletedAt"].is_number() {
        return Err("会话已删除".into());
    }
    Ok(s)
}
pub async fn save_session(db: &mut SqliteConnection, value: &mut Value) -> Result<(), String> {
    value["updatedAt"] = json!(now());
    value["revision"] = json!(value["revision"].as_u64().unwrap_or(0) + 1);
    put(
        db,
        NS,
        &format!("session:{}", str_field(value, "id")?),
        value,
    )
    .await
}
pub fn message(role: &str, text: &str) -> Value {
    json!({"id":id(),"role":role,"text":text,"createdAt":now()})
}

/// 每轮在首次结束或等待作者时封存时间；重放完成事件不能把作者等待时间计入用时。
pub fn finish_reply(s: &mut Value, run: &str) {
    if let Some(reply) = s["messages"].as_array_mut().and_then(|messages| {
        messages
            .iter_mut()
            .find(|m| m["id"] == format!("{run}:reply"))
    }) {
        if let Some(started) = reply["createdAt"].as_i64() {
            if reply["finishedAt"].is_null() {
                reply["finishedAt"] = json!(now().max(started));
            }
        }
    }
}

/// 通用测试使用作者已有作品；Agent 建书采用边界由专门的流程测试覆盖。
#[cfg(test)]
pub async fn fixture_existing_book(
    db: &mut SqliteConnection,
    s: &mut Value,
    title: &str,
) -> Result<Value, String> {
    let bid = crate::agent_service::create_book(db, title).await?;
    s["targetBookId"] = json!(bid);
    s["contextBookId"] = json!(bid);
    s["references"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":format!("book:{bid}"),"bookId":bid,"kind":"book","title":title}));
    save_session(db, s).await?;
    let entries = catalog(db, &bid).await?;
    Ok(
        json!({"bookId":bid,"chapterId":entries[0]["id"].to_string(),"volumeId":entries[0]["volumeId"],"title":title}),
    )
}

pub async fn book(db: &mut SqliteConnection, book_id: &str) -> Result<Value, String> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM local_books WHERE CAST(id AS TEXT)=? AND deletedAt IS NULL",
    )
    .bind(book_id)
    .fetch_optional(db)
    .await
    .map_err(err)?;
    serde_json::from_str(&raw.ok_or("作品不存在或已删除")?).map_err(err)
}
pub async fn chapter(
    db: &mut SqliteConnection,
    book_id: &str,
    chapter_id: &str,
) -> Result<Value, String> {
    book(db, book_id).await?;
    let catalog: Option<String> = sqlx::query_scalar("SELECT payload FROM local_chapters WHERE bookId=? AND CAST(id AS TEXT)=? AND deletedAt IS NULL")
        .bind(book_id).bind(chapter_id).fetch_optional(&mut *db).await.map_err(err)?;
    let catalog: Value =
        serde_json::from_str(&catalog.ok_or("章节不存在或不属于当前作品")?).map_err(err)?;
    let key = format!("guest:{book_id}:{chapter_id}");
    let raw: Option<String> =
        sqlx::query_scalar("SELECT payload FROM chapter_contents WHERE storageKey=?")
            .bind(&key)
            .fetch_optional(db)
            .await
            .map_err(err)?;
    if let Some(raw) = raw {
        return serde_json::from_str(&raw).map_err(err);
    }
    Ok(
        json!({"storageKey":key,"userId":"guest","bookId":book_id,"chapterId":catalog["id"],
        "title":catalog["title"],"textContent":"","contentJson":null,"localVersion":0,
        "remoteVersion":0,"baseRemoteVersion":0,"baseTitle":"","baseTextContent":"",
        "baseContentJson":null,"dirty":true,"conflict":false,"localOnly":true,"updatedAt":0,"lastBackedUpAt":0}),
    )
}

/// Agent 和编辑器共用章节版本字段与历史表。正文、目录统计、回执在同一事务提交。
#[cfg(test)]
pub async fn save_chapter(
    root: &Path,
    book_id: &str,
    chapter_id: &str,
    text: &str,
    expected: i64,
    operation: &str,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let result = save_chapter_in(&mut tx, book_id, chapter_id, text, expected, operation).await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}

/// 可加入成果应用事务，使正文、历史、回执和候选状态一次提交。
pub async fn save_chapter_in(
    db: &mut SqliteConnection,
    book_id: &str,
    chapter_id: &str,
    text: &str,
    expected: i64,
    operation: &str,
) -> Result<Value, String> {
    if text.len() > 240_000 {
        return Err("章节内容超过单次修改上限".into());
    }
    let request = json!([book_id, chapter_id, text, expected]);
    if let Some(old) = get(&mut *db, OP_NS, operation).await? {
        if old["request"] != request {
            return Err("重复操作编号对应不同内容".into());
        }
        return Ok(old["result"].clone());
    }
    let mut current = chapter(&mut *db, book_id, chapter_id).await?;
    let version = current["localVersion"].as_i64().unwrap_or(0);
    if version != expected {
        return Err(format!(
            "版本冲突：读取版本 {expected}，当前版本 {version}；未覆盖最新正文"
        ));
    }
    let stamp = now();
    let original = current["textContent"].as_str().unwrap_or("").to_owned();
    let history = json!({"id":operation,"userId":"guest","bookId":book_id,"chapterId":current["chapterId"],
        "source":"local","title":current["title"],"textContent":original,"contentJson":current["contentJson"],
        "remoteVersion":current["remoteVersion"],"remark":"Agent 修改前快照","createdAt":stamp});
    sqlx::query("INSERT INTO chapter_versions(id,payload,chapterId,createdAt) VALUES(?,?,?,?)")
        .bind(operation)
        .bind(history.to_string())
        .bind(chapter_id)
        .bind(stamp)
        .execute(&mut *db)
        .await
        .map_err(err)?;
    // 纯文本成果转换为编辑器文档，不能保留与新正文不一致的旧 contentJson。
    current["textContent"] = json!(text);
    current["contentJson"] = json!({"type":"doc","content":text.split('\n').map(|line|
        if line.is_empty() { json!({"type":"paragraph"}) } else {
            json!({"type":"paragraph","content":[{"type":"text","text":line}]}) }).collect::<Vec<_>>()});
    current["localVersion"] = json!(version + 1);
    current["dirty"] = json!(true);
    current["workflowPreview"] = json!(false);
    current["updatedAt"] = json!(stamp);
    let words = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{feff}')
        .map(|c| c.len_utf16())
        .sum::<usize>() as i64;
    let text_words = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.len_utf16())
        .sum::<usize>() as i64;
    sqlx::query("INSERT INTO chapter_contents(storageKey,userId,bookId,chapterId,payload,dirty,conflict,updatedAt,lastBackedUpAt,wordCount,textWordCount) VALUES(?,?,?,?,?,1,0,?,?,?,?) ON CONFLICT(storageKey) DO UPDATE SET payload=excluded.payload,dirty=1,updatedAt=excluded.updatedAt,wordCount=excluded.wordCount,textWordCount=excluded.textWordCount")
        .bind(current["storageKey"].as_str().unwrap()).bind("guest").bind(book_id).bind(chapter_id)
        .bind(current.to_string()).bind(stamp).bind(current["lastBackedUpAt"].as_i64().unwrap_or(0))
        .bind(words).bind(text_words).execute(&mut *db).await.map_err(err)?;
    sqlx::query("UPDATE local_chapters SET payload=json_set(payload,'$.wordCount',?,'$.textWordCount',?,'$.updateTime',?) WHERE CAST(id AS TEXT)=? AND bookId=?")
        .bind(words).bind(text_words).bind(chrono::Utc::now().to_rfc3339()).bind(chapter_id).bind(book_id).execute(&mut *db).await.map_err(err)?;
    sqlx::query("DELETE FROM sync_settings WHERE key LIKE 'bookWordCounts:%'")
        .execute(&mut *db)
        .await
        .map_err(err)?;
    sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.wordCount',COALESCE((SELECT SUM(COALESCE(json_extract(payload,'$.wordCount'),0)) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),0),'$.chapterCount',(SELECT COUNT(*) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(book_id).bind(book_id).bind(chrono::Utc::now().to_rfc3339()).bind(chrono::Utc::now().to_rfc3339()).bind(book_id)
        .execute(&mut *db).await.map_err(err)?;
    let result = json!({"saved":true,"bookId":book_id,"chapterId":chapter_id,"version":version+1,"original":original,"content":text});
    put(
        &mut *db,
        OP_NS,
        operation,
        &json!({"request":request,"result":result}),
    )
    .await?;
    // 来源版本变化后立即失效；自动重新提取属于独立记忆任务，不在正文事务中调用模型。
    for mut m in list(&mut *db, NS, "memory:").await? {
        if m["bookId"] == book_id && m["chapterId"] == chapter_id && m["sourceVersion"] == expected
        {
            m["status"] = json!("archived");
            crate::agent_memory::store(&mut *db, &m).await?;
        }
    }
    Ok(result)
}

pub async fn catalog(db: &mut SqliteConnection, book_id: &str) -> Result<Vec<Value>, String> {
    book(db, book_id).await?;
    let rows: Vec<String> = sqlx::query_scalar("SELECT c.payload FROM local_chapters c LEFT JOIN local_volumes v ON CAST(v.id AS TEXT)=c.volumeId AND v.bookId=c.bookId WHERE c.bookId=? AND c.deletedAt IS NULL ORDER BY COALESCE(v.sortNo,0),c.sortNo,c.id")
        .bind(book_id).fetch_all(db).await.map_err(err)?;
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(err))
        .collect()
}

/// 新项目允许先构思；一旦绑定作品即沿用真实 bookId，不将 projectId 当作书籍编号。
pub async fn project(
    db: &mut SqliteConnection,
    title: &str,
    genre: &str,
    book_id: Option<&str>,
) -> Result<Value, String> {
    if title.trim().is_empty() || title.chars().count() > 60 {
        return Err("项目名称应为 1—60 字".into());
    }
    if let Some(book_id) = book_id {
        book(db, book_id).await?;
    }
    let p = json!({"id":id(),"title":title.trim(),"genre":genre,"bookId":book_id,"chapters":0,"createdAt":now()});
    put(db, NS, &format!("project:{}", str_field(&p, "id")?), &p).await?;
    Ok(p)
}

pub fn new_session(project_id: Option<&str>, title: &str) -> Value {
    json!({"id":id(),"projectId":project_id,"title":title,"scenario":"review","status":"completed",
        "step":0,"draft":"","references":[],"messages":[],"choiceId":"","targetBookId":null,"scopeVersion":2,"revision":0,"createdAt":now(),"updatedAt":now()})
}
