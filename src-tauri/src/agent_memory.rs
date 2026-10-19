//! 作品记忆是带来源版本的派生记录，不能成为第二份正式正文。
//! 读取时验证来源，兼容原编辑器的写入；不依赖某一条 Agent 保存路径触发失效。
use crate::{agent_creation as creation, agent_service as service, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::{collections::BTreeSet, path::Path};

fn source_ref(raw: &Value) -> Result<Value, String> {
    let bid = str_field(raw, "bookId")?;
    Ok(match str_field(raw, "type")? {
        "chapter" => json!({"type":"chapter","bookId":bid,"chapterId":str_field(raw,"chapterId")?}),
        "material" => {
            json!({"type":"material","bookId":bid,"materialKind":str_field(raw,"materialKind")?,"entityId":raw["entityId"].as_i64().ok_or("缺少资料编号")?})
        }
        "author" => {
            json!({"type":"author","bookId":bid,"sessionId":str_field(raw,"sessionId")?,"messageId":str_field(raw,"messageId")?})
        }
        "artifact" => {
            json!({"type":"artifact","bookId":bid,"artifactId":str_field(raw,"artifactId")?})
        }
        _ => return Err("记忆来源类型无效".into()),
    })
}

/// 记忆的唯一写入口：落记录并同步检索索引。所有改状态、改内容、归档、删除都走这里。
pub async fn store(db: &mut SqliteConnection, memory: &Value) -> Result<(), String> {
    put(db, NS, &format!("memory:{}", str_field(memory, "id")?), memory).await?;
    crate::agent_indexer::sync_memory_index(db, memory).await
}

pub async fn context_book(db: &mut SqliteConnection, s: &Value) -> Result<Option<String>, String> {
    let p = service::session_project(db, s).await?;
    let mut allowed = BTreeSet::new();
    for bid in [s["targetBookId"].as_str(), p["bookId"].as_str()]
        .into_iter()
        .flatten()
    {
        if !bid.is_empty() {
            allowed.insert(bid.to_owned());
        }
    }
    for r in s["references"].as_array().into_iter().flatten() {
        // 拆书项目是参考书，不作为作品上下文
        if r["kind"] != "material" && r["kind"] != "breakdown" {
            if let Some(bid) = r["bookId"].as_str().filter(|v| !v.is_empty()) {
                allowed.insert(bid.to_owned());
            }
        }
    }
    // 自动上下文只选仍存在的作品。项目或历史会话中的旧关联不能阻断无关的只读任务。
    // 显式作品读写继续经过 allowed_book / book 校验，不能借此绕过已删除作品的保护。
    let mut live = BTreeSet::new();
    for bid in allowed {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM local_books WHERE CAST(id AS TEXT)=? AND deletedAt IS NULL)",
        )
        .bind(&bid)
        .fetch_one(&mut *db)
        .await
        .map_err(err)?;
        if exists {
            live.insert(bid);
        }
    }
    let allowed = live;
    for bid in [
        s["targetBookId"].as_str(),
        s["contextBookId"].as_str(),
        p["bookId"].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        if allowed.contains(bid) {
            return Ok(Some(bid.to_owned()));
        }
    }
    Ok(if allowed.len() == 1 {
        allowed.into_iter().next()
    } else {
        None
    })
}

/// 返回源内容及指纹；指纹不是数据库格式版本，也不根据时间戳猜测内容是否改变。
/// 摘录使用资料的可见文字；原实体仍用于版本指纹，存储元数据不当作创作内容。
fn material_source_text(entity: &Value, kind: &str) -> Result<String, String> {
    let fields = entity.as_object().ok_or("原资料格式错误")?;
    let mut parts = Vec::new();
    for (key, value) in fields {
        if matches!(key.as_str(), "id" | "bookId" | "parentId" | "createdAt" | "updatedAt" | "createTime" | "updateTime" | "sourcePlanId") { continue; }
        let Some(text) = value.as_str() else { continue; };
        if kind == "outline" && key == "content" {
            let mut html = text.to_owned();
            for tag in ["</p>", "</div>", "</h1>", "</h2>", "</h3>", "</h4>", "</li>", "<br>", "<br/>", "<br />"] { html = html.replace(tag, "\n"); }
            parts.push(crate::agent_context::strip_html(&html).replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&"));
        } else { parts.push(text.to_owned()); }
    }
    Ok(parts.join("\n"))
}

pub async fn source_value(
    db: &mut SqliteConnection,
    source: &Value,
) -> Result<(String, String), String> {
    match source["type"].as_str().unwrap_or("") {
        "chapter" => {
            // 参考书（拆书项目）的章节同样按标题与正文计算来源指纹
            if let Some(pid) = crate::agent_breakdown::project_of(str_field(source, "bookId")?) {
                let c = crate::agent_breakdown::chapter(db, pid, str_field(source, "chapterId")?).await?;
                return Ok((
                    crate::agent_breakdown::version_of(&c),
                    c["textContent"].as_str().unwrap_or("").to_owned(),
                ));
            }
            let c = chapter(
                db,
                str_field(source, "bookId")?,
                str_field(source, "chapterId")?,
            )
            .await?;
            Ok((
                creation::version(&json!([c["localVersion"], c["title"], c["textContent"]])),
                c["textContent"].as_str().unwrap_or("").to_owned(),
            ))
        }
        "material" => {
            let doc = creation::materials(db, str_field(source, "bookId")?).await?;
            let collection = creation::collection(str_field(source, "materialKind")?)?;
            let entity = doc[collection]
                .as_array()
                .and_then(|a| a.iter().find(|v| v["id"] == source["entityId"]))
                .ok_or("原资料已删除")?;
            let text = material_source_text(entity, str_field(source, "materialKind")?)?;
            Ok((creation::version(entity), text))
        }
        "author" => {
            // 对话软删除不抹去作品决定的证据，仍保留作者原话供审计。
            let s = get(
                db,
                NS,
                &format!("session:{}", str_field(source, "sessionId")?),
            )
            .await?
            .ok_or("来源会话不存在")?;
            let m = s["messages"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|m| m["id"] == source["messageId"] && m["role"] == "user")
                })
                .ok_or("作者原话不存在")?;
            let text = str_field(m, "text")?.to_owned();
            Ok((creation::version(&json!(text)), text))
        }
        "artifact" => {
            let a = get(
                db,
                NS,
                &format!("artifact:{}", str_field(source, "artifactId")?),
            )
            .await?
            .ok_or("原成果不存在")?;
            if a["bookId"] != source["bookId"] {
                return Err("成果不属于当前作品".into());
            }
            Ok((
                creation::version(&json!([a["revision"], a["content"], a["applied"]])),
                a["content"].as_str().unwrap_or("").to_owned(),
            ))
        }
        _ => Err("不支持的记忆来源".into()),
    }
}

pub fn source_receipt_key(source: &Value) -> String {
    if source["type"] == "author" {
        format!("memory-source:{}:{}:{}", source["bookId"].as_str().unwrap_or(""), source["sessionId"].as_str().unwrap_or(""), source["messageId"].as_str().unwrap_or(""))
    } else {
        format!("memory-source:{}:{}", source["bookId"].as_str().unwrap_or(""), creation::version(source))
    }
}

/// Re-index a source by semantic role without altering its original message or
/// canonical story data. Only derived memories from this exact source can retire.
pub async fn reconcile_author_projection(db: &mut SqliteConnection, source: &Value, text: &str, entry: &Value) -> Result<(),String> {
    let projection = &entry["authorProjection"];
    crate::agent_contract::validate_document("taskProjection",projection)?;
    let segments = projection["segments"].as_array().ok_or("作者来源分类缺少分段")?;
    let compact = |s: &str| s.chars().filter(|c|!c.is_whitespace()).collect::<String>();
    let joined = segments.iter().filter_map(|s|s["text"].as_str()).collect::<Vec<_>>().join("");
    if compact(&joined) != compact(text) { return Err("作者来源分类未逐字覆盖原文".into()); }
    let retained = entry["retainedMemoryIds"].as_array().ok_or("缺少本次保留的派生记忆编号")?;
    for mut memory in list(db,NS,"memory:").await? {
        if memory["sourceRef"] != *source || memory["deletedAt"].is_number() || retained.contains(&memory["id"]) { continue; }
        memory["status"] = json!("archived");
        memory["deletedAt"] = json!(now());
        memory["revision"] = json!(memory["revision"].as_u64().unwrap_or(0)+1);
        memory["retiredReason"] = json!("来源已按创作要求与平台操作重新整理");
        store(db,&memory).await?;
    }
    Ok(())
}

pub async fn refresh(
    db: &mut SqliteConnection,
    book_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let mut records = list(db, NS, "memory:").await?;
    for m in &mut records {
        if m["deletedAt"].is_number()
            || !["active", "pending"].contains(&m["status"].as_str().unwrap_or(""))
        {
            continue;
        }
        if book_id.is_some_and(|bid| m["scope"] == "book" && m["bookId"] != bid) {
            continue;
        }
        let stale = if m["sourceRef"].is_object() {
            match source_value(db, &m["sourceRef"]).await {
                Ok((v, _)) => m["sourceRevision"] != v,
                Err(_) => true,
            }
        } else if m["chapterId"].is_string() && m["sourceVersion"].is_number() {
            // 兼容旧版已带章节来源的记忆；作者手工记忆没有来源指纹，仍按原状态使用。
            match chapter(db, str_field(m, "bookId")?, str_field(m, "chapterId")?).await {
                Ok(c) => c["localVersion"] != m["sourceVersion"],
                Err(_) => true,
            }
        } else {
            false
        };
        if stale {
            m["status"] = json!("stale");
            m["staleReason"] = json!("原文或资料已改变，需要重新核对");
            m["revision"] = json!(m["revision"].as_u64().unwrap_or(0) + 1);
            m["updatedAt"] = json!(now());
            store(db, m).await?;
        }
    }
    Ok(records)
}

/// 长篇大纲按来源规模提供摘要空间；其他来源维持短记忆上限，总上下文预算不变。
pub(crate) fn summary_limit(source: &Value, text: &str) -> usize {
    if source["type"] == "material" && source["materialKind"] == "outline" {
        (text.chars().count() / 2).clamp(1200, 4000)
    } else { 1200 }
}

pub async fn save_manual(db: &mut SqliteConnection, args: &Value) -> Result<Value, String> {
    let content = str_field(args, "content")?.trim();
    let status = str_field(args, "status")?;
    if !["active", "pending", "rejected", "archived"].contains(&status) {
        return Err("请核对内容后选择有效、待定、否决或归档".into());
    }
    let scope = str_field(args, "scope")?;
    if !["book", "author"].contains(&scope) {
        return Err("记忆范围无效".into());
    }
    let bid = if scope == "book" {
        let bid = str_field(args, "bookId")?;
        book(db, bid).await?;
        bid
    } else {
        ""
    };
    let memory_id = args["id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(id);
    let key = format!("memory:{memory_id}");
    let old = get(db, NS, &key).await?;
    let limit = old.as_ref().and_then(|m| m["contentLimit"].as_u64()).unwrap_or(1200).clamp(1200, 4000) as usize;
    if content.is_empty() || content.chars().count() > limit {
        return Err(format!("记忆内容应为 1—{limit} 字"));
    }
    if let Some(old) = &old {
        if old["deletedAt"].is_number()
            || old["revision"].as_u64().unwrap_or(0) != args["revision"].as_u64().unwrap_or(0)
        {
            return Err("记忆已更新，请重新读取后修改".into());
        }
        put(
            db,
            OP_NS,
            &format!(
                "memory-edit:{memory_id}:{}",
                old["revision"].as_u64().unwrap_or(0)
            ),
            old,
        )
        .await?;
    }
    let category = args["category"].as_str().unwrap_or(if scope == "author" { "作者偏好" } else { "作者决定" });
    let mut m = json!({"id":memory_id,"bookId":bid,"content":content,"category":category,
        "kind":"decision","scope":scope,"status":status,"contentLimit":limit,"source":"作者手动确认","quote":args["quote"].as_str().unwrap_or(""),
        "channel":args["channel"].as_str().unwrap_or("desktop"),
        "observedAt":old.as_ref().and_then(|v|v["observedAt"].as_i64()).unwrap_or_else(now),
        "period":args["period"].as_str().unwrap_or("全书"),"revision":old.as_ref().map_or(0,|v|v["revision"].as_u64().unwrap_or(0)+1),
        "previousSource":old.as_ref().map(|v|v["sourceRef"].clone()),"previousSourceRevision":old.as_ref().map(|v|v["sourceRevision"].clone()),"createdAt":old.as_ref().map_or(now(),|v|v["createdAt"].as_i64().unwrap_or(now())),"updatedAt":now()});
    if let Some(old) = old.filter(|v| {
        v["kind"] == "summary"
            && v["content"] == content
            && v["bookId"] == bid
            && ["active", "pending"].contains(&status)
    }) {
        let (current, _) = source_value(db, &old["sourceRef"]).await?;
        if old["sourceRevision"] != current {
            return Err("来源已变化，不能直接确认旧摘要；请先回读原文并核对内容".into());
        }
        for key in ["kind", "sourceRef", "sourceRevision", "source", "partial"] {
            m[key] = old[key].clone();
        }
    }
    store(db, &m).await?;
    Ok(m)
}

pub async fn record(root: &Path, sid: &str, args: &Value) -> Result<Value, String> {
    use crate::agent_tool_boundary::{failure, FailureKind};
    let reject = |code: &str, kind: FailureKind, message: &str, recovery: &str| {
        Ok(failure(code, kind, message, true, recovery, json!({})))
    };
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    service::allowed_book(&mut tx, &s, bid, false).await?;
    let op = format!("memory:{sid}:{}", str_field(args, "operation_id")?);
    if let Some(done) = get(&mut tx, OP_NS, &op).await? {
        if done.get("request").is_some() {
            if done["request"] != *args {
                return Ok(failure("operation_intent_mismatch", FailureKind::Conflict, "重复记忆操作编号对应不同内容，请先读取已有记忆。", false, "recall_memory", json!({})));
            }
            return Ok(done["result"].clone());
        }
        return Ok(done);
    }
    let kind = str_field(args, "kind")?;
    let mut status = str_field(args, "status")?;
    if !["decision", "summary", "candidate"].contains(&kind)
        || !["active", "pending", "rejected"].contains(&status)
    {
        return reject("memory_state_invalid", FailureKind::Precondition, "记忆类型或状态无效", "");
    }
    // 跨作品的作者偏好：只能是作者原话里的决定，不能是摘要或候选
    let scope = args["scope"].as_str().unwrap_or("book");
    if !["book", "author"].contains(&scope) {
        return reject("memory_state_invalid", FailureKind::Precondition, "记忆范围无效", "");
    }
    let normalized_source = source_ref(&args["source_ref"])?;
    let source = &normalized_source;
    if source["bookId"] != bid {
        return reject("reference_owner_mismatch", FailureKind::Reference, "记忆来源不能跨作品混用", "get_memory_sources");
    }
    let author = source["type"] == "author";
    if author && source["sessionId"] != sid {
        return Ok(failure("reference_out_of_scope", FailureKind::Permission, "创作决定必须来自本会话作者原话", false, "get_memory_sources", json!({})));
    }
    if author && context_book(&mut tx, &s).await?.as_deref() != Some(bid) {
        return reject("memory_context_required", FailureKind::Precondition, "请先确定当前创作作品，再记录作者决定", "load_book_context");
    }
    if (kind == "decision" || status == "rejected") && !author {
        return reject("memory_author_evidence_required", FailureKind::Precondition, "采用或否决设定必须有作者原话依据", "get_memory_sources");
    }
    if scope == "author" && (!author || kind != "decision") {
        return reject("memory_author_evidence_required", FailureKind::Precondition, "跨作品的作者偏好只能来自作者原话里的决定", "get_memory_sources");
    }
    if (kind == "candidate" || source["type"] == "artifact") && status != "pending" {
        return reject("memory_state_invalid", FailureKind::Precondition, "候选成果只能作为待定记忆", "");
    }
    if kind == "summary"
        && !["chapter", "material"].contains(&source["type"].as_str().unwrap_or(""))
    {
        return reject("memory_source_kind_mismatch", FailureKind::Reference, "摘要只能来自正式正文或资料", "get_memory_sources");
    }
    let (revision, text) = source_value(&mut tx, source).await?;
    if args["source_revision"] != revision {
        return reject("memory_source_stale", FailureKind::Conflict, "来源已更新，旧内容不能形成有效记忆", "get_memory_sources");
    }
    let quote = str_field(args, "quote")?.trim();
    if quote.is_empty() || quote.chars().count() > 600 || !text.contains(quote) {
        return reject("memory_quote_mismatch", FailureKind::Precondition, "记忆必须附带来源中实际存在的原文摘录", "get_memory_sources");
    }
    let content = str_field(args, "content")?.trim();
    let limit = summary_limit(source, &text);
    if content.is_empty() || content.chars().count() > limit {
        return reject("memory_content_invalid", FailureKind::Precondition, &format!("该来源的记忆内容应为 1—{limit} 字"), "");
    }
    let records = list(&mut tx, NS, "memory:").await?;
    let authored_at = if author {
        s["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find(|m| m["id"] == source["messageId"]))
            .and_then(|m| m["createdAt"].as_i64())
            .unwrap_or(0)
    } else {
        0
    };
    // 两个会话同时整理决定时，较旧作者原话不能晚到后重新激活旧方案。
    if author
        && status != "pending"
        && records.iter().any(|m| {
            (if scope == "author" { m["scope"] == "author" } else { m["bookId"] == bid && m["scope"] != "author" })
                && m["kind"] == "decision"
                && !m["deletedAt"].is_number()
                && ["active", "rejected"].contains(&m["status"].as_str().unwrap_or(""))
                && m["authoredAt"]
                    .as_i64()
                    .unwrap_or_else(|| m["updatedAt"].as_i64().unwrap_or(0))
                    > authored_at
        })
    {
        status = "pending";
    }
    // 同一来源的同版本摘要不反复生成；人工删除后也不自动复活。
    if let Some(existing) = records.iter().find(|m| {
        m["sourceRef"] == *source
            && m["sourceRevision"] == revision
            && m["kind"] == kind
            && m["scope"].as_str().unwrap_or("book") == scope
            && (kind == "summary" || m["content"] == content)
    }) {
        return Ok(json!({"saved":true,"memoryId":existing["id"],"status":existing["status"]}));
    }
    let mid = id();
    if kind == "summary" {
        for mut old in records
            .iter()
            .filter(|m| {
                m["sourceRef"] == *source && m["kind"] == "summary" && m["status"] == "stale"
            })
            .cloned()
        {
            old["status"] = json!("archived");
            old["replacedBy"] = json!(mid);
            old["revision"] = json!(old["revision"].as_u64().unwrap_or(0) + 1);
            store(&mut tx, &old).await?;
        }
    }
    let supersedes = args["supersedes"].as_array().cloned().unwrap_or_default();
    if !supersedes.is_empty() && (!author || kind != "decision" || status == "pending") {
        return reject("memory_author_evidence_required", FailureKind::Precondition, "只有明确作者决定可以替代旧记忆", "get_memory_sources");
    }
    for replacement in &supersedes {
        let old_id = str_field(replacement, "id")?;
        let Some(mut old) = get(&mut tx, NS, &format!("memory:{old_id}")).await? else {
            return reject("memory_not_found", FailureKind::Reference, "被替代的记忆不存在", "recall_memory");
        };
        let same_scope = if scope == "author" { old["scope"] == "author" } else { old["bookId"] == bid && old["scope"] == "book" };
        if !same_scope
            || old["deletedAt"].is_number()
            || old["revision"].as_u64().unwrap_or(0)
                != replacement["revision"]
                    .as_u64()
                    .ok_or("缺少被替代记忆版本")?
        {
            return reject("memory_revision_conflict", FailureKind::Conflict, "旧记忆已经变化或不属于当前作品", "recall_memory");
        }
        old["previousStatus"] = old["status"].clone();
        old["status"] = json!("archived");
        old["replacedBy"] = json!(mid);
        old["revision"] = json!(old["revision"].as_u64().unwrap_or(0) + 1);
        old["updatedAt"] = json!(now());
        old["changedBySessionId"] = json!(sid);
        old["changedByRunId"] = s["runId"].clone();
        store(&mut tx, &old).await?;
    }
    let label = match source["type"].as_str().unwrap_or("") {
        "author" if scope == "author" => "作者偏好",
        "author" => "作者对话决定",
        "chapter" => "章节摘要",
        "material" => "正式资料摘要",
        _ => "候选方案",
    };
    let source_label = if source["type"] == "chapter" {
        let c = chapter(&mut tx, bid, str_field(source, "chapterId")?).await?;
        format!(
            "《{}》 · 版本 {}",
            c["title"].as_str().unwrap_or("章节"),
            c["localVersion"]
        )
    } else if author {
        format!("对话《{}》", s["title"].as_str().unwrap_or("创作对话"))
    } else {
        label.to_owned()
    };
    // 作者偏好不属于某本书，bookId 留空；观察日期取作者说这话的时间
    let memory = json!({"id":mid,"bookId":if scope == "author" { "" } else { bid },"scope":scope,"kind":kind,"status":status,"content":content,
        "category":label,"source":source_label,"quote":quote,"period":args["period"].as_str().unwrap_or("全书"),
        "sourceRef":source,"sourceRevision":revision,"sessionId":sid,"channel":crate::agent_channel::of_session(&s)["kind"],
        "observedAt":if authored_at > 0 { authored_at } else { now() },
        "supersedes":supersedes,"partial":text.chars().count()>12000,"contentLimit":limit,
        "revision":0,"createdAt":now(),"updatedAt":now(),"authoredAt":authored_at,"changedBySessionId":sid,"changedByRunId":s["runId"]});
    store(&mut tx, &memory).await?;
    let result = json!({"saved":true,"memoryId":mid,"status":status});
    put(
        &mut tx,
        OP_NS,
        &op,
        &json!({"request":args,"result":result}),
    )
    .await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}

pub async fn context(
    db: &mut SqliteConnection,
    s: &Value,
    bid: Option<&str>,
) -> Result<Value, String> {
    let bid = match bid {
        Some(v) => Some(v.to_owned()),
        None => context_book(db, s).await?,
    };
    let all = refresh(db, bid.as_deref()).await?;
    let allowed = all
        .into_iter()
        .filter(|m| {
            !m["deletedAt"].is_number()
                && (m["scope"] == "author" || bid.as_ref().is_some_and(|bid| m["bookId"] == *bid))
        })
        .collect::<Vec<_>>();
    let mut active = allowed
        .iter()
        .filter(|m| m["status"] == "active")
        .cloned()
        .collect::<Vec<_>>();
    let mut rejected = allowed
        .iter()
        .filter(|m| m["status"] == "rejected")
        .cloned()
        .collect::<Vec<_>>();
    let mut pending = allowed
        .iter()
        .filter(|m| m["status"] == "pending")
        .cloned()
        .collect::<Vec<_>>();
    let stale = allowed
        .iter()
        .filter(|m| m["status"] == "stale")
        .map(|m| json!({"id":m["id"],"sourceRef":m["sourceRef"],"reason":m["staleReason"]}))
        .collect::<Vec<_>>();
    let total = active.len() + rejected.len() + pending.len();
    fn compact(items: &mut Vec<Value>, budget: usize) {
        items.sort_by_key(|m| {
            (
                if m["kind"] == "decision" { 0 } else { 1 },
                -m["updatedAt"].as_i64().unwrap_or(0),
            )
        });
        let mut used = 0;
        items.retain(|m| {
            used += m["content"].as_str().unwrap_or("").chars().count();
            used <= budget
        });
        for m in items {
            *m = json!({"id":m["id"],"kind":m["kind"],"status":m["status"],"content":m["content"],"revision":m["revision"],"period":m["period"],"sourceRef":m["sourceRef"],"partial":m["partial"]});
        }
    }
    compact(&mut active, 12000);
    // 否决约束不能因摘要预算而静默丢弃；只压缩其展示字段。
    compact(&mut rejected, usize::MAX);
    compact(&mut pending, 3000);
    let omitted = total - active.len() - rejected.len() - pending.len();
    let mut out = json!({"book":null,"active":active,"rejected":rejected,"pending":pending,"stale":stale,"recentTasks":[],"chapters":[],"omittedMemoryCount":omitted});
    // 最近几天的工作备注（作者交代的待办、约定好下次要做的事）
    out["journal"] = json!(crate::agent_journal::recent_notes(db).await?);
    if let Some(bid) = bid {
        service::allowed_book(db, s, &bid, false).await?;
        let b = book(db, &bid).await?;
        out["book"] = json!({"id":bid,"title":b["title"]});
        let catalog = catalog(db, &bid).await?;
        let mut chapters = Vec::new();
        for c in catalog.iter().rev().take(8).rev() {
            let body = chapter(db, &bid, &c["id"].to_string()).await?;
            chapters.push(json!({"id":c["id"],"title":c["title"],"volumeId":c["volumeId"],"localVersion":body["localVersion"],"hasContent":!body["textContent"].as_str().unwrap_or("").is_empty()}));
        }
        out["chapters"] = json!(chapters);
        out["chapterCount"] = json!(catalog.len());
        let mut tasks = Vec::new();
        for previous in list(db, NS, "session:").await? {
            if previous["id"] == s["id"] || previous["deletedAt"].is_number() {
                continue;
            }
            if context_book(db, &previous).await.ok().flatten().as_deref() != Some(&bid) {
                continue;
            }
            let last_user = previous["messages"]
                .as_array()
                .and_then(|a| a.iter().rev().find(|m| m["role"] == "user"));
            let question = previous["messages"]
                .as_array()
                .and_then(|a| a.iter().rev().find(|m| m["choice"]["status"] == "pending"));
            tasks.push(json!({"sessionId":previous["id"],"title":previous["title"],"status":previous["status"],"updatedAt":previous["updatedAt"],
                "lastRequest":last_user.and_then(|m|m["text"].as_str()).unwrap_or("").chars().take(600).collect::<String>(),"waitingQuestion":question.map(|m|m["text"].clone())}));
        }
        tasks.sort_by_key(|v| -v["updatedAt"].as_i64().unwrap_or(0));
        tasks.truncate(5);
        out["recentTasks"] = json!(tasks);
    }
    Ok(out)
}

pub async fn extraction_sources(
    db: &mut SqliteConnection,
    s: &Value,
    phase: &str,
) -> Result<Value, String> {
    let Some(bid) = context_book(db, s).await? else {
        return Ok(json!({"sources":[],"memories":[]}));
    };
    service::allowed_book(db, s, &bid, false).await?;
    let memories = refresh(db, Some(&bid)).await?;
    let mut sources = Vec::new();
    let retry_authors = s["memoryStatus"].as_object().into_iter().flat_map(|m| m.values())
        .flat_map(|p| p["pendingSources"].as_array().into_iter().flatten().chain(p["failures"].as_array().into_iter().flatten().map(|f| &f["source"])))
        .filter(|source| source["type"] == "author" && source["bookId"] == bid)
        .filter_map(|source| source["messageId"].as_str()).collect::<BTreeSet<_>>();
    let legacy_authors: BTreeSet<&str> = memories.iter()
        .filter(|m|!m["deletedAt"].is_number() && m["sourceRef"]["type"] == "author" && m["sourceRef"]["sessionId"] == s["id"])
        .filter_map(|m|m["sourceRef"]["messageId"].as_str()).collect();
    // 消息携带实际 runId；附件/模型回复不参与作者决定提取。
    for m in s["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "user" && (m["runId"] == s["runId"] || m["id"].as_str().is_some_and(|id| retry_authors.contains(id) || legacy_authors.contains(id))))
    {
        let source = json!({"type":"author","bookId":bid,"sessionId":s["id"],"messageId":m["id"]});
        let (revision, text) = source_value(db, &source).await?;
        if phase != "inspect"
            && get(
                db,
                OP_NS,
                &format!(
                    "memory-source:{bid}:{}:{}",
                    str_field(s, "id")?,
                    str_field(m, "id")?
                ),
            )
            .await?
            .is_some_and(|v| v["revision"] == revision && v["semanticVersion"] == 2)
        {
            continue;
        }
        sources.push(json!({"ref":source,"revision":revision,"text":text,"projectionRequired":true}));
    }
    if phase != "author" {
        let doc = creation::materials(db, &bid).await?;
        let mut refs = memories
            .iter()
            .filter(|m| {
                m["bookId"] == bid
                    && m["status"] == "stale"
                    && ["chapter", "material"]
                        .contains(&m["sourceRef"]["type"].as_str().unwrap_or(""))
            })
            .map(|m| m["sourceRef"].clone())
            .collect::<Vec<_>>();
        for read in list(db, OP_NS, &format!("read:{}:{bid}:", str_field(s, "id")?))
            .await?
            .iter()
            .filter(|v| v["readRunId"] == s["runId"])
        {
            refs.push(
                json!({"type":"chapter","bookId":bid,"chapterId":read["chapterId"].to_string()}),
            );
        }
        for c in catalog(db, &bid).await?.iter().rev().take(3) {
            refs.push(json!({"type":"chapter","bookId":bid,"chapterId":c["id"].to_string()}));
        }
        for (kind, key) in [
            ("character", "characters"),
            ("outline", "outlineNodes"),
            ("world_setting", "worldSettings"),
        ] {
            for entity in doc[key].as_array().into_iter().flatten() {
                refs.push(json!({"type":"material","bookId":bid,"materialKind":kind,"entityId":entity["id"]}));
            }
        }
        // 有限批次，不把整本作品塞入一次模型请求；未整理的来源下次继续处理。
        let mut seen = BTreeSet::new();
        for source in refs {
            if sources.len() >= 8 {
                break;
            }
            if !seen.insert(source.to_string()) {
                continue;
            }
            let Ok((revision, text)) = source_value(db, &source).await else {
                continue;
            };
            let receipt = get(db, OP_NS, &source_receipt_key(&source)).await?;
            if phase != "inspect" && receipt.is_some_and(|r| r["revision"] == revision) { continue; }
            if text.trim().is_empty()
                || (phase != "inspect"
                    && memories.iter().any(|v| {
                        (v["previousSource"] == source && v["previousSourceRevision"] == revision)
                            || (v["sourceRef"] == source
                                && v["sourceRevision"] == revision
                                && v["kind"] == "summary")
                    }))
            {
                continue;
            }
            sources.push(json!({"ref":source,"revision":revision,"text":text,"summaryLimit":summary_limit(&source,&text)}));
            if sources.len() >= 8 {
                break;
            }
        }
    }
    let previous=memories.iter().filter(|m|m["bookId"]==bid && !m["deletedAt"].is_number() && ["active","pending","rejected"].contains(&m["status"].as_str().unwrap_or("")))
        .map(|m|json!({"id":m["id"],"revision":m["revision"].as_u64().unwrap_or(0),"kind":m["kind"],"status":m["status"],"content":m["content"]})).take(80).collect::<Vec<_>>();
    Ok(json!({"bookId":bid,"sources":sources,"memories":previous}))
}

async fn decisions(db: &mut SqliteConnection, bid: &str) -> Result<Vec<Value>, String> {
    Ok(list(db, NS, "memory:").await?.into_iter()
        .filter(|m| (m["scope"] == "author" || m["bookId"] == bid) && m["kind"] != "summary"
            && !m["deletedAt"].is_number() && m["status"] != "archived")
        .map(|m| json!({"id":m["id"],"revision":m["revision"],"status":m["status"],"deletedAt":m["deletedAt"],
            "changedBySessionId":m["changedBySessionId"],"changedByRunId":m["changedByRunId"]})).collect())
}

/// 捕获本次模型实际看到的上下文；检查点不承担正式作品的版本判断。
pub async fn capture_context(
    db: &mut SqliteConnection,
    s: &Value,
    run: &str,
    context: &mut Value,
) -> Result<(), String> {
    let Some(bid) = context["book"]["id"].as_str() else {
        return Ok(());
    };
    let mut checks = Vec::new();
    for c in context["chapters"].as_array().into_iter().flatten() {
        let source = json!({"type":"chapter","bookId":bid,"chapterId":c["id"].to_string()});
        let (revision, _) = source_value(db, &source).await?;
        checks.push(json!({"source":source,"revision":revision}));
    }
    let sid = str_field(s, "id")?;
    let mut changed_sources = Vec::new();
    for read in list(db, OP_NS, &format!("read:{sid}:{bid}:"))
        .await?
        .iter()
        .filter(|r| r["readRunId"] == run)
    {
        let source =
            json!({"type":"chapter","bookId":bid,"chapterId":read["chapterId"].to_string()});
        let revision = creation::version(&json!([
            read["localVersion"],
            read["title"],
            read["textContent"]
        ]));
        if source_value(db, &source)
            .await
            .map_or(true, |current| current.0 != revision)
        {
            changed_sources.push(source.clone());
        }
        let check = json!({"source":source,"revision":revision});
        if let Some(old) = checks.iter_mut().find(|c| c["source"] == check["source"]) {
            *old = check;
        } else {
            checks.push(check);
        }
    }
    let current_materials = creation::story_version(&creation::materials(db, bid).await?);
    let read = get(db, OP_NS, &format!("material-context:{sid}:{bid}")).await?;
    let materials = read
        .filter(|r| r["runId"] == run)
        .and_then(|r| r["revision"].as_str().map(str::to_owned))
        .unwrap_or_else(|| current_materials.clone());
    if materials != current_materials {
        changed_sources.push(json!({"type":"materials","bookId":bid}));
    }
    let proof = json!({"bookId":bid,"chapters":checks,
        "materials":materials,"decisions":decisions(db,bid).await?,"sessionId":s["id"],"runId":run});
    put(
        db,
        OP_NS,
        &format!("context:{}:{run}", str_field(s, "id")?),
        &proof,
    )
    .await?;
    context["changedSources"] = json!(changed_sources);
    Ok(())
}

pub async fn verify_context(
    db: &mut SqliteConnection,
    sid: &str,
    run: &str,
    bid: &str,
) -> Result<(), String> {
    let Some(proof) = get(db, OP_NS, &format!("context:{sid}:{run}")).await? else {
        return Ok(());
    };
    verify_proof(db, &proof, bid).await
}

pub async fn verify_proof(
    db: &mut SqliteConnection,
    proof: &Value,
    bid: &str,
) -> Result<(), String> {
    let mut changed = proof["bookId"] != bid
        || !creation::same_story(&proof["materials"], &creation::materials(db, bid).await?);
    let current = decisions(db, bid).await?;
    let original = proof["decisions"].as_array().into_iter().flatten()
        .filter(|m|!m["deletedAt"].is_number() && m["status"] != "archived").cloned().collect::<Vec<_>>();
    for m in &current {
        // 当前任务自己记录的作者要求已在模型输入中，不因记忆整理完成而误判候选过期。
        if m["changedBySessionId"] == proof["sessionId"]
            && m["changedByRunId"] == proof["runId"]
            && m["changedByRunId"].is_string()
        {
            continue;
        }
        let old = original.iter().find(|v| v["id"] == m["id"]);
        changed |= old != Some(m);
    }
    changed |= original
        .iter()
        .any(|v| !current.iter().any(|m| m["id"] == v["id"]));
    for check in proof["chapters"].as_array().into_iter().flatten() {
        changed |= match source_value(db, &check["source"]).await {
            Ok((revision, _)) => check["revision"] != revision,
            Err(_) => true,
        };
    }
    if changed {
        return Err(
            "创作上下文已变化：相关章节、设定或记忆在生成期间被修改，请保留候选并重新读取后再写入"
                .into(),
        );
    }
    Ok(())
}
