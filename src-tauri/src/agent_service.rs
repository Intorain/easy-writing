//! Agent 业务接口。所有读写在宿主执行，Python 只能调用这里公开的领域操作。
use super::agent_store::{self as store, *};
use crate::agent_book_plan as book_plan;
use crate::agent_creation as creation;
use crate::agent_memory as memory;
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::path::Path;

pub async fn workspace(root: &Path) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    {
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        memory::refresh(&mut tx, None).await?;
        crate::agent_breakdown_job::restore_conversation_cards(&mut tx).await?;
        crate::agent_distillation::sync_scope_interactions(&mut tx).await?;
        tx.commit().await.map_err(err)?;
    }
    // 旧内置技能"章节创作与修订"已由文件技能"章节写作"接替（agent_skills::RETIRED_BY_FILES），不再在这里补建
    let records = list(&mut db, NS, "").await?;
    let mut projects = Vec::new();
    let mut sessions = Vec::new();
    let mut artifacts = Vec::new();
    let mut memories = Vec::new();
    let mut skills = Vec::new();
    // 集合按记录 kind 区分；旧存档的项目/会话也使用同一身份，禁止按标题重新建副本。
    for r in records {
        if r["deletedAt"].is_number() {
            continue;
        }
        if r.get("messages").is_some() {
            let mut r=r;
            r["draftReferences"]=crate::agent_prompt::draft_references(&r);
            sessions.push(r);
        } else if r.get("original").is_some() {
            artifacts.push(book_plan::with_progress(r));
        } else if r.get("category").is_some() {
            memories.push(r);
        } else if r.get("instructions").is_some() {
            // 被文件技能接替的旧技能不再列出：列出来作者会去开，开了又和接替者抢写作方法
            if r["retiredAt"].is_number() {
                continue;
            }
            skills.push(r);
        } else if r.get("genre").is_some() {
            projects.push(r);
        }
    }
    sessions.sort_by_key(|s| -s["updatedAt"].as_i64().unwrap_or(0));
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT payload FROM local_books WHERE deletedAt IS NULL ORDER BY updateTime DESC",
    )
    .fetch_all(&mut db)
    .await
    .map_err(err)?;
    let mut references = Vec::new();
    let mut live_books = std::collections::HashSet::new();
    for raw in rows {
        let b: Value = serde_json::from_str(&raw).map_err(err)?;
        let bid = b["id"].to_string();
        live_books.insert(bid.clone());
        references.push(json!({"id":format!("book:{bid}"),"bookId":bid,"kind":"book","title":b["title"],"description":format!("{} 章",b["chapterCount"].as_i64().unwrap_or(0))}));
        let chapters = catalog(&mut db, &bid).await?;
        for v in volumes(&mut db, &bid).await? {
            let vid = v["id"].to_string();
            let count = chapters.iter().filter(|c| c["volumeId"].to_string().trim_matches('"') == vid).count();
            references.push(json!({"id":format!("volume:{vid}"),"bookId":bid,"volumeId":vid,"kind":"volume","title":v["title"],
                "description":format!("{} · {count} 章", b["title"].as_str().unwrap_or(""))}));
        }
        for c in &chapters {
            let cid = c["id"].to_string();
            references.push(json!({"id":format!("chapter:{cid}"),"bookId":bid,"chapterId":cid,"kind":"chapter","title":c["title"],"description":b["title"]}));
        }
        // 单个人物、单条设定：引用时带上它的资料，不必整本书资料都读
        let doc = crate::agent_creation::materials(&mut db, &bid).await?;
        for (collection, entity_type, label) in [("characters", "character", "人物"), ("worldSettings", "world_setting", "设定")] {
            for e in doc[collection].as_array().into_iter().flatten() {
                let Some(name) = e["name"].as_str().or(e["title"].as_str()).filter(|n| !n.trim().is_empty()) else {
                    continue;
                };
                let eid = e["id"].to_string().trim_matches('"').to_owned();
                references.push(json!({"id":format!("entity:{bid}:{entity_type}:{eid}"),"bookId":bid,"kind":"entity","entityType":entity_type,
                    "entityId":eid,"title":name,"description":format!("{label} · {}", b["title"].as_str().unwrap_or(""))}));
            }
        }
    }
    // 榜单快照：本地抓取过的最新若干份
    for snap in crate::agent_rank::recent_snapshots(&mut db, 30).await? {
        let (sid, date) = (snap["sourceId"].as_i64().unwrap_or(0), snap["statDate"].as_str().unwrap_or(""));
        let name = snap["pageTitle"].as_str().filter(|t| !t.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("榜单 {sid}"));
        references.push(json!({"id":format!("rank:{sid}:{date}"),"bookId":"","kind":"rank","sourceId":sid,"statDate":date,
            "title":format!("{name} {date}"),"description":format!("榜单快照 · {} 本", snap["itemCount"].as_u64().unwrap_or(0))}));
    }
    // 拆书项目作为参考书可以引用；编号带 breakdown: 前缀，不会被当成作者作品
    for p in crate::agent_breakdown::list_projects(&mut db).await? {
        let pid = p["projectId"].as_str().unwrap_or("").to_owned();
        let indexed = p["cards"].as_u64().unwrap_or(0);
        let description = if indexed > 0 { format!("参考书 · {} 章 · 已拆 {indexed} 章", p["chapters"]) } else { format!("参考书 · {} 章", p["chapters"]) };
        references.push(json!({"id":crate::agent_breakdown::book_id(&pid),"bookId":crate::agent_breakdown::book_id(&pid),"kind":"breakdown",
            "title":p["title"],"description":description}));
    }
    for p in &mut projects {
        if let Some(bid) = p["bookId"].as_str() {
            if let Ok(b) = book(&mut db, bid).await {
                p["chapters"] = b["chapterCount"].clone();
            }
        }
    }
    // 引用的作品进了回收站：对话照样列出、能看，列表里标出来；彻底删除时引用会被摘掉（book_purged），标记随之消失
    for s in &mut sessions {
        let missing = s["references"].as_array().is_some_and(|refs| {
            refs.iter().any(|r| r["kind"] == "book" && r["bookId"].as_str().is_some_and(|bid| !live_books.contains(bid)))
        });
        if missing {
            s["bookDeleted"] = json!(true);
        }
    }
    Ok(
        json!({"projects":projects,"conversations":sessions,"artifacts":artifacts,"memories":memories,"skills":skills,"references":references,"distillationJobs":crate::agent_distillation::list_views(&mut db).await?}),
    )
}

pub async fn command(root: &Path, action: &str, args: &Value) -> Result<Value, String> {
    if action.starts_with("reference_") { return crate::agent_reference_import::command(root, action, args).await; }
    if action == "breakdown_results" || action == "breakdown_library" {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        if action == "breakdown_library" { return Ok(json!(crate::agent_breakdown::list_projects(&mut db).await?)); }
        let pid=str_field(args,"projectId")?;
        let mut result=crate::agent_breakdown::results_in_range(&mut db,pid,args["offset"].as_u64().unwrap_or(0) as usize,args["from"].as_u64().unwrap_or(1) as usize,args["to"].as_u64().unwrap_or(0) as usize).await?;
        if let Some(sid)=args["sessionId"].as_str().filter(|s|!s.is_empty()) {
            result["report"]=crate::agent_breakdown::conversation_report(&mut db,pid,sid,result["rangeFrom"].as_u64().unwrap_or(1) as usize,result["rangeTo"].as_u64().unwrap_or(0) as usize).await?;
            result["legacyReport"]=Value::Null;
        }
        return Ok(result);
    }
    let normalized = crate::agent_contract::canonical_arguments(args);
    let args = &normalized;
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let value = match action {
        "writing_profiles" => json!(crate::agent_canon::platform_profiles(&mut tx).await?),
        // ---- Agent 拆书：导入整本参考书、预估、任务列表；开始 / 暂停 / 取消由宿主调度通道 ----
        "material_put" => put_material(&mut tx, args["title"].as_str().unwrap_or("本地资料"), args["content"].as_str().unwrap_or("")).await?,
        "breakdown_import" => crate::agent_breakdown::import_book(&mut tx, str_field(args, "title")?, &args["chapters"],
            args["sourceHash"].as_str().unwrap_or(""), &args["notes"]).await?,
        "breakdown_estimate" => crate::agent_breakdown_job::estimate(&mut tx, &crate::agent_breakdown::project_id_arg(args)?,
            args["from"].as_i64().unwrap_or(1), args["to"].as_i64().unwrap_or(0)).await?,
        "breakdown_jobs" => json!(crate::agent_breakdown_job::list_views(&mut tx).await?),
        "distillation_start" => {
            let model=model_config(&mut tx,"","agent_indexer").await?;
            crate::agent_distillation::start(&mut tx,str_field(args,"id")?,str_field(args,"mode")?,args["from"].as_u64().unwrap_or(1) as usize,args["to"].as_u64().unwrap_or(0) as usize,&model).await?
        },
        "distillation_control" => crate::agent_distillation::control(&mut tx,str_field(args,"id")?,str_field(args,"control")?).await?,
        "distillation_read" => {
            if let Some(eid)=args["evidenceRef"].as_str().filter(|v|!v.is_empty()) {crate::agent_distillation::read_evidence(&mut tx,str_field(args,"id")?,eid).await?}
            else {crate::agent_distillation::read_results(&mut tx,str_field(args,"id")?,args["offset"].as_u64().unwrap_or(0) as usize,20).await?}
        },
        "create_project" => {
            let request = str_field(args, "requestId")?;
            if let Some(done) = get(&mut tx, OP_NS, &format!("project:{request}")).await? {
                return active_project(&mut tx, str_field(&done, "id")?).await;
            }
            // 创建项目只建立组织空间。即使旧前端仍传 createBook，也不产生书籍。
            let p = project(&mut tx, str_field(args, "title")?, "", None).await?;
            put(&mut tx, OP_NS, &format!("project:{request}"), &p).await?;
            p
        }
        "queue_message" => {
            let mut s = session(&mut tx, str_field(args, "id")?).await?;
            if s["status"] != "running" {
                return Err("任务已结束，请重新发送补充要求".into());
            }
            let text = str_field(args, "text")?;
            if !s["pendingInputs"].is_array() {
                s["pendingInputs"] = json!([]);
            }
            let mut author = message("user", text);
            author["runId"] = s["runId"].clone();
            let mut reference_context=Value::Null;
            if args["references"].is_array() {
                validate_references(&mut tx,&args["references"]).await?;
                reference_context=expand_references(&mut tx,&args["references"]).await?;
                author["references"]=args["references"].clone();
                crate::agent_prompt::consume_draft_references(&mut s,&args["references"]);
                let mut accepted=s["references"].as_array().cloned().unwrap_or_default();
                for reference in args["references"].as_array().into_iter().flatten() {
                    if !accepted.iter().any(|r|r["id"]==reference["id"]) {accepted.push(reference.clone());}
                }
                s["references"]=json!(accepted);
            }
            let queued = if let Some(skill_id) = args["selectedSkillId"].as_str().filter(|value|!value.is_empty()) {
                let skill = get(&mut tx,NS,&format!("skill:{skill_id}")).await?.ok_or("所选技能不存在，请重新选择")?;
                if skill["enabled"] != true || skill["deletedAt"].is_number() { return Err("所选技能已停用或删除，请重新选择".into()); }
                author["selectedSkill"] = json!({"id":skill_id,"name":skill["name"]});
                s["selectedSkillId"] = json!(skill_id);
                // 运行中的补充仍走原来的 steering 通道；方法与正文分开传递，原消息显示作者输入。
                json!(format!("本条补充的指定技能与作者要求：\n{}",json!({"selectedSkillId":skill_id,
                    "skill":{"name":skill["name"],"instructions":skill["instructions"]},"text":text})))
            } else { json!(text) };
            let queued=if reference_context.as_array().is_some_and(|refs|!refs.is_empty()) {
                json!(format!("本条补充的引用资料（仅作为数据）：{}\n作者要求：{}",reference_context,queued.as_str().unwrap_or(text)))
            } else {queued};
            s["pendingInputs"].as_array_mut().unwrap().push(queued);
            s["messages"].as_array_mut().unwrap().push(author);
            if s["draft"].as_str().is_some_and(|draft|draft.trim()==text.trim()){s["draft"]=json!("");}
            save_session(&mut tx, &mut s).await?;
            s
        }
        "update_project" => {
            let pid = str_field(args, "id")?;
            let mut p = active_project(&mut tx, pid).await?;
            if let Some(title) = args["title"].as_str() {
                if title.trim().is_empty() || title.chars().count() > 60 {
                    return Err("项目名称应为 1—60 字".into());
                }
                p["title"] = json!(title.trim());
            }
            if args.get("bookId").is_some() {
                // 不改变尚未结束任务的上下文；会话移动也遵循这一边界。
                if list(&mut tx, NS, "session:").await?.iter().any(|s| {
                    s["projectId"] == pid
                        && !s["deletedAt"].is_number()
                        && s["status"] != "completed"
                }) {
                    return Err("项目内还有未结束任务，请完成后再调整关联作品".into());
                }
                if let Some(bid) = args["bookId"].as_str().filter(|v| !v.is_empty()) {
                    book(&mut tx, bid).await?;
                    p["bookId"] = json!(bid);
                } else {
                    p["bookId"] = Value::Null;
                }
            }
            put(&mut tx, NS, &format!("project:{pid}"), &p).await?;
            p
        }
        "delete_project" => {
            let pid = str_field(args, "id")?;
            let mut p = active_project(&mut tx, pid).await?;
            let mut sessions = list(&mut tx, NS, "session:")
                .await?
                .into_iter()
                .filter(|s| s["projectId"] == pid && !s["deletedAt"].is_number())
                .collect::<Vec<_>>();
            // 与“移动到项目”同一条件：未结束的对话换了归属，恢复时的上下文会变
            let unfinished = sessions.iter().filter(|s| s["status"] != "completed").count();
            if unfinished > 0 {
                return Err(format!(
                    "项目内还有 {unfinished} 个未结束的对话（运行中、暂停、等待回答或失败待恢复），请先处理后再删除项目"
                ));
            }
            let ids = sessions
                .iter()
                .filter_map(|s| s["id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            if crate::agent_batch::list_all(&mut tx).await?.iter().any(|j| {
                j["sessionId"].as_str().is_some_and(|sid| ids.iter().any(|id| id == sid))
                    && ["queued", "running", "waiting", "paused"]
                        .contains(&j["status"].as_str().unwrap_or(""))
            }) {
                return Err("项目内还有未结束的批量连载，请完成或取消后再删除项目".into());
            }
            // 对话移到独立会话：只改组织归属，引用、成果、检查点和写入目标保持原身份
            for s in sessions.iter_mut() {
                s["projectId"] = Value::Null;
                save_session(&mut tx, s).await?;
            }
            // 软删除，备份仍包含记录；关联作品和作品记忆不属于项目，不受影响
            p["deletedAt"] = json!(now());
            put(&mut tx, NS, &format!("project:{pid}"), &p).await?;
            json!({"id":pid,"deletedAt":p["deletedAt"],"movedSessions":ids})
        }
        "move_session" => {
            let mut s = session(&mut tx, str_field(args, "id")?).await?;
            if s["status"] != "completed" {
                return Err("请完成当前任务后再调整会话归属".into());
            }
            let pid = args["projectId"].as_str().filter(|v| !v.is_empty());
            if let Some(pid) = pid {
                active_project(&mut tx, pid).await?;
            }
            // 只修改组织归属。引用、成果、检查点和明确指定的写入目标均保持原身份。
            s["projectId"] = json!(pid);
            save_session(&mut tx, &mut s).await?;
            s
        }
        "create_session" => {
            let request = args["requestId"].as_str().map(|id| format!("session:{id}"));
            if let Some(key) = &request {
                if let Some(existing) = get(&mut tx, OP_NS, key).await? {
                    return session(&mut tx, str_field(&existing, "id")?).await;
                }
            }
            let pid = args["projectId"].as_str().filter(|id| !id.is_empty());
            if let Some(pid) = pid {
                active_project(&mut tx, pid).await?;
            }
            let mut s = new_session(pid, args["title"].as_str().unwrap_or("新对话"));
            if let Some(draft) = args["draft"].as_str() {
                s["draft"] = json!(draft);
            }
            if args["references"].is_array() {
                validate_references(&mut tx, &args["references"]).await?;
                s["references"] = args["references"].clone();
            }
            s["draftReferences"]=s["references"].clone();
            // 入口声明自己的通道档案；桌面入口不传，读取时按桌面处理
            if !args["channel"].is_null() {
                s["channel"] = crate::agent_channel::normalize(&args["channel"])?;
            }
            save_session(&mut tx, &mut s).await?;
            if let Some(key) = request {
                put(&mut tx, OP_NS, &key, &s).await?;
            }
            s
        }
        // 旧版妙笔的对话记录搬进会话：按旧对话编号去重，只读（legacy 标记），作者看得见、能继续开新对话
        "import_legacy_chats" => {
            let bid = str_field(args, "bookId")?;
            let b = book(&mut tx, bid).await?;
            let title = b["title"].as_str().unwrap_or("").to_owned();
            let mut imported = 0usize;
            for chat in args["sessions"].as_array().ok_or("缺少旧对话")? {
                let old_id = str_field(chat, "id")?;
                let key = format!("session:miaobi-legacy:{old_id}");
                if get(&mut tx, OP_NS, &key).await?.is_some() {
                    continue;
                }
                let messages: Vec<Value> = chat["messages"].as_array().into_iter().flatten()
                    .filter(|m| m["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
                    .map(|m| json!({"id":id(),"role":if m["role"] == "user" { "user" } else { "assistant" },
                        "text":m["text"].as_str().unwrap_or("").chars().take(100_000).collect::<String>(),
                        "createdAt":m["createdAt"].as_i64().unwrap_or_else(now)}))
                    .collect();
                if messages.is_empty() {
                    continue;
                }
                let stamp = chat["updatedAt"].as_i64().unwrap_or_else(now);
                let mut s = new_session(None, chat["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("妙笔对话"));
                s["legacy"] = json!("miaobi");
                s["references"] = json!([{"id":format!("book:{bid}"),"kind":"book","bookId":bid,"title":title,"description":""}]);
                s["messages"] = json!(messages);
                s["createdAt"] = json!(chat["createdAt"].as_i64().unwrap_or(stamp));
                save_session(&mut tx, &mut s).await?;
                // 保持旧对话原来的时间，列表里按当时的先后排，不冒充新对话
                s["updatedAt"] = json!(stamp);
                put(&mut tx, NS, &format!("session:{}", str_field(&s, "id")?), &s).await?;
                put(&mut tx, OP_NS, &key, &json!({"id":s["id"],"bookId":bid})).await?;
                imported += 1;
            }
            json!({"imported":imported})
        }
        "update_session" => {
            let mut s = session(&mut tx, str_field(args, "id")?).await?;
            if let Some(pinned) = args.get("pinned") {
                let pinned = pinned.as_bool().ok_or("置顶状态无效")?;
                // 置顶属于会话元数据；重复置顶保留原顺序，不修改项目归属或作品数据。
                s["pinnedAt"] = if pinned {
                    json!(s["pinnedAt"].as_i64().unwrap_or_else(now))
                } else {
                    Value::Null
                };
            }
            if let Some(title) = args["title"].as_str() {
                s["title"] = json!(title.chars().take(60).collect::<String>());
            }
            if let Some(draft) = args["draft"].as_str() {
                s["draft"] = json!(draft);
            }
            if !args["channel"].is_null() {
                s["channel"] = crate::agent_channel::normalize(&args["channel"])?;
            }
            // 引导卡里作者选到一半、填到一半的内容，退出重开后还在；提交回答时清空
            if s["status"] == "choice" && args.get("promptDraft").is_some_and(|d| d.is_object() || d.is_null()) {
                s["promptDraft"] = args["promptDraft"].clone();
            }
            if args["draftReferences"].is_array() {
                validate_references(&mut tx,&args["draftReferences"]).await?;
                s["draftReferences"]=crate::agent_prompt::update_draft_references(&s,&args["draftReferences"],args.get("draftBaseMessageId"));
            }
            if s["status"] != "running" {
                if args["references"].is_array() {
                    validate_references(&mut tx, &args["references"]).await?;
                    s["references"] = args["references"].clone();
                    s["draftReferences"] = args["references"].clone();
                }
            }
            save_session(&mut tx, &mut s).await?;
            s
        }
        // 回收站"彻底删除"作品：这本书的妙笔会话一起删（软删除，与"删除会话"同口径）；只是引用过它的对话摘掉引用、清掉写入目标
        "book_purged" => {
            let bid = str_field(args, "bookId")?;
            let about_book = |r: &Value| r["kind"] != "material" && r["bookId"] == bid;
            let (mut deleted, mut detached) = (0usize, 0usize);
            for mut s in list(&mut tx, NS, "session:").await? {
                if s["deletedAt"].is_number() {
                    continue;
                }
                let references_book = s["references"].as_array().is_some_and(|refs| refs.iter().any(about_book));
                if references_book && s["channel"]["label"] == "妙笔" && s["status"] != "running" {
                    s["deletedAt"] = json!(now());
                    save_session(&mut tx, &mut s).await?;
                    deleted += 1;
                    continue;
                }
                let mut changed = false;
                if s["draftReferences"].as_array().is_some_and(|refs|refs.iter().any(about_book)) {
                    s["draftReferences"]=json!(s["draftReferences"].as_array().unwrap().iter().filter(|r|!about_book(r)).cloned().collect::<Vec<_>>());
                    changed=true;
                }
                if references_book {
                    let kept: Vec<Value> = s["references"].as_array().unwrap().iter().filter(|r| !about_book(r)).cloned().collect();
                    s["references"] = json!(kept);
                    changed = true;
                }
                for field in ["targetBookId", "contextBookId"] {
                    if s[field] == bid {
                        s[field] = Value::Null;
                        changed = true;
                    }
                }
                if s["readSources"].as_array().is_some_and(|v| v.iter().any(|x| x == bid)) {
                    let rest: Vec<Value> = s["readSources"].as_array().unwrap().iter().filter(|x| *x != bid).cloned().collect();
                    s["readSources"] = json!(rest);
                    changed = true;
                }
                if changed {
                    save_session(&mut tx, &mut s).await?;
                    detached += 1;
                }
            }
            json!({"deletedSessions":deleted,"detachedSessions":detached})
        }
        "delete_session" => {
            let mut s = session(&mut tx, str_field(args, "id")?).await?;
            if s["status"] == "running" {
                return Err("请先停止当前任务再删除会话".into());
            }
            // 软删除，保留检查点与审计来源；备份仍包含记录。
            s["deletedAt"] = json!(now());
            save_session(&mut tx, &mut s).await?;
            s
        }
        // 工作备注页：有过活动的日期、某一天的工作、删一条备注
        "journal_days" => json!(crate::agent_journal::days(&mut tx, args["limit"].as_u64().unwrap_or(60) as usize).await?),
        "journal_day" => crate::agent_journal::day(&mut tx, str_field(args, "date")?, args["bookId"].as_str().filter(|v| !v.is_empty())).await?,
        "journal_delete" => json!({"deleted":crate::agent_journal::delete_note(&mut tx, str_field(args, "id")?).await?}),
        // 自动任务页：任务、运行记录、事件规则、投递队列
        "apply_artifact_basis" => apply_basis(&mut tx, str_field(args, "id")?).await?,
        "automation_list" => json!(crate::agent_scheduler::list_tasks(&mut tx).await?),
        "automation_update" => crate::agent_scheduler::update_task(&mut tx, str_field(args, "id")?, args).await?,
        "automation_delete" => json!({"deleted":crate::agent_scheduler::delete_task(&mut tx, str_field(args, "id")?).await?}),
        "automation_runs" => json!(crate::agent_scheduler::runs(&mut tx, str_field(args, "id")?, args["limit"].as_u64().unwrap_or(30) as usize).await?),
        "automation_rules" => json!(crate::agent_scheduler::rules(&mut tx).await?),
        "automation_rule_update" => crate::agent_scheduler::rule_update(&mut tx, str_field(args, "event")?, args).await?,
        "automation_settings" => crate::agent_scheduler::settings(&mut tx).await?,
        "automation_settings_update" => crate::agent_scheduler::settings_update(&mut tx, args).await?,
        "delivery_pending" => json!(crate::agent_delivery_queue::pending(&mut tx, args["channel"].as_str().unwrap_or("")).await?),
        "delivery_recent" => json!(crate::agent_delivery_queue::recent(&mut tx, args["limit"].as_u64().unwrap_or(50) as usize).await?),
        "delivery_ack" => crate::agent_delivery_queue::ack(&mut tx, str_field(args, "id")?).await?,
        // 忘掉这次会话：归档它派生的记忆（离开索引）、删它的检查点与短期回执、清掉未完成状态；原始对话记录与成果不删
        "forget_session" => {
            let sid = str_field(args, "id")?;
            let mut s = session(&mut tx, sid).await?;
            if s["status"] == "running" {
                return Err("请先停止当前任务再忘掉这次会话".into());
            }
            let mut forgotten = 0usize;
            for mut m in list(&mut tx, NS, "memory:").await? {
                let derived = m["sessionId"] == sid || m["sourceRef"]["sessionId"] == sid;
                if !derived || m["deletedAt"].is_number() {
                    continue;
                }
                m["previousStatus"] = m["status"].clone();
                m["status"] = json!("archived");
                m["deletedAt"] = json!(now());
                m["forgottenAt"] = json!(now());
                m["revision"] = json!(m["revision"].as_u64().unwrap_or(0) + 1);
                m["updatedAt"] = json!(now());
                memory::store(&mut tx, &m).await?;
                forgotten += 1;
            }
            // 检查点：主线程 `{sid}:…`，子线程 `{sid}|…`
            let mut checkpoints = 0u64;
            for prefix in [format!("{sid}:"), format!("{sid}|")] {
                checkpoints += delete_by_prefix(&mut tx, CHECKPOINT_NS, &prefix).await?;
            }
            // 短期回执：读取记录、意图绑定、审批、目录/作品/网页桥/榜单回执、记忆整理状态等
            let mut receipts = 0u64;
            for prefix in ["read:", "read-materials:", "context:", "candidate-context:", "material-context:", "tool-intent:", "approval:",
                "catalog:", "library:", "webview:", "rank:", "rank-confirm:", "memory:", "memory-status:", "skill-consent-token:",
                "skill-consent:", "skill-draft:", "book-plan-consent:", "revise-scope:"] {
                receipts += delete_by_prefix(&mut tx, OP_NS, &format!("{prefix}{sid}")).await?;
            }
            for key in ["pendingInputs", "memoryStatus"] {
                s[key] = json!(if key == "pendingInputs" { json!([]) } else { json!({}) });
            }
            s["memoryWarning"] = Value::Null;
            s["promptDraft"] = Value::Null;
            s["draft"] = json!("");
            s["targetBookId"] = Value::Null;
            s["forgottenAt"] = json!(now());
            save_session(&mut tx, &mut s).await?;
            json!({"session":s,"forgottenMemories":forgotten,"checkpoints":checkpoints,"receipts":receipts})
        }
        // 通道适配器只需要一条会话或一份成果，不用整个工作区
        "get_session" => session(&mut tx, str_field(args, "id")?).await?,
        "get_artifact" => get(&mut tx, NS, &format!("artifact:{}", str_field(args, "id")?)).await?.ok_or("成果不存在")?,
        "save_memory" => memory::save_manual(&mut tx, args).await?,
        "canon_mark" => crate::agent_story::mark(&mut tx, str_field(args, "bookId")?, str_field(args, "recordKey")?, str_field(args, "mark")?).await?,
        "add_note" => crate::agent_story::add_note(&mut tx, args).await?,
        "delete_note" => crate::agent_story::delete_note(&mut tx, str_field(args, "id")?).await?,
        "import_legacy_notes" => crate::agent_story::import_legacy_notes(&mut tx, str_field(args, "bookId")?, &args["notes"]).await?,
        "record_reading_choice" => crate::agent_eval::record_reading_choice(&mut tx, args).await?,
        // 方法包：作者在界面上选的文件路径；只读写这一个文件
        "method_pack_export" => {
            let pack = crate::agent_method_pack::export(&mut tx).await?;
            let path = str_field(args, "path")?;
            std::fs::write(path, serde_json::to_vec_pretty(&pack).map_err(err)?).map_err(|e| format!("写入方法包失败：{e}"))?;
            let n = |k: &str| pack[k].as_array().map_or(0, |a| a.len());
            json!({"path":path,"rules":n("rules"),"profiles":n("profiles"),"recipes":n("recipes")})
        }
        "method_pack_import" => {
            let path = str_field(args, "path")?;
            if std::fs::metadata(path).map_err(|e| format!("读取方法包失败：{e}"))?.len() > 5 * 1024 * 1024 {
                return Err("方法包文件超过 5MB，不像是易创导出的方法包".into());
            }
            let pack: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("读取方法包失败：{e}"))?)
                .map_err(|_| "方法包不是有效的 JSON 文件".to_owned())?;
            crate::agent_method_pack::import(&mut tx, &pack).await?
        }
        "template_recipes" => json!(crate::agent_method_pack::template_recipes(&mut tx).await?),
        "apply_digest" => crate::agent_digest::apply(&mut tx, str_field(args, "bookId")?).await?,
        // ---- 作品状态层：作者维护的数据 ----
        "save_rule" => {
            crate::agent_canon::save_rule(
                &mut tx,
                str_field(args, "scope")?,
                args["bookId"].as_str(),
                str_field(args, "category")?,
                str_field(args, "text")?,
                "作者手动",
            )
            .await?
        }
        "delete_rule" => {
            crate::agent_canon::delete_rule(&mut tx, str_field(args, "id")?).await?;
            json!(true)
        }
        "save_platform_profile" => {
            crate::agent_canon::save_platform_profile(&mut tx, &args["profile"]).await?
        }
        "set_book_profile" => {
            crate::agent_canon::set_book_profile(
                &mut tx,
                str_field(args, "bookId")?,
                str_field(args, "profileId")?,
            )
            .await?
        }
        "confirm_samples" => {
            let ids = args["chapterIds"]
                .as_array()
                .ok_or("缺少章节列表")?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            let bid = str_field(args, "bookId")?;
            let mut record = crate::agent_canon::confirm_samples(&mut tx, bid, &ids).await?;
            // 切块来源随确认更新并重算风格档案
            record["style"] = crate::agent_style::apply_samples(&mut tx, bid, &ids).await?;
            record
        }
        "style_rebuild" => crate::agent_style::rebuild(&mut tx, str_field(args, "bookId")?).await?,
        "record_ai_spans" => {
            crate::agent_canon::record_ai_spans(&mut tx, str_field(args, "bookId")?, str_field(args, "chapterId")?, &args["spans"]).await?
        }
        "style_lock" => {
            crate::agent_style::lock_points(&mut tx, str_field(args, "bookId")?, args["points"].as_str().unwrap_or("")).await?
        }
        "impact_report" => {
            crate::agent_canon::impact_report(&mut tx, str_field(args, "bookId")?, str_field(args, "chapterId")?).await?
        }
        "save_verdict" => {
            crate::agent_canon::record_verdict(
                &mut tx,
                str_field(args, "bookId")?,
                str_field(args, "issueKey")?,
                str_field(args, "decision")?,
                args["note"].as_str().unwrap_or(""),
            )
            .await?
        }
        "override_thread" => {
            crate::agent_canon::override_thread(
                &mut tx,
                str_field(args, "bookId")?,
                str_field(args, "threadId")?,
                str_field(args, "status")?,
                args["note"].as_str().unwrap_or(""),
            )
            .await?
        }
        "context" => {
            // 写作台、工作流生文调用：不依赖会话，预算由调用方按所用模型传入
            let model=model_config(&mut tx,args["modelCode"].as_str().unwrap_or(""),"text_assist").await.ok();
            let hints = args["charactersHint"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>())
                .unwrap_or_default();
            let out = crate::agent_context::assemble(
                &mut tx,
                None,
                &crate::agent_context::ContextRequest {
                    book_id: str_field(args, "bookId")?,
                    purpose: args["purpose"].as_str().unwrap_or("write"),
                    chapter_id: args["chapterId"].as_str().filter(|v| !v.is_empty()),
                    ruleset_id: args["rulesetId"].as_str().filter(|v| !v.is_empty()),
                    query_hint: args["queryHint"].as_str().filter(|v| !v.is_empty()),
                    characters_hint: hints,
                    model_max_context: args["maxContext"].as_u64().or_else(||model.as_ref().and_then(|m|m["maxContext"].as_u64())).unwrap_or(32000) as usize,
                    output_reserve: args["outputReserve"].as_u64().or_else(||model.as_ref().and_then(|m|m["maxOutputTokens"].as_u64())).unwrap_or(2048) as usize,
                    budget_tokens: args["budgetTokens"].as_u64().filter(|v| *v > 0).map(|v| v as usize),
                },
            )
            .await?;
            let mut out = out;
            if out["ok"] == true {
                out["rendered"] = json!(crate::agent_context::render(&out));
            }
            out
        }
        "quality_check" => {
            // 写作台“本章体检”：传正文则检查该稿，否则读取正式正文；报告绑定章节版本与文本指纹
            let bid = str_field(args, "bookId")?;
            let chapter_id = args["chapterId"].as_str().filter(|v| !v.is_empty());
            let (text, revision, artifact_id) = match (args["text"].as_str(), chapter_id) {
                (Some(text), Some(cid)) => {
                    let body = chapter(&mut tx, bid, cid).await?;
                    (
                        text.to_owned(),
                        body["localVersion"].as_i64().unwrap_or(0),
                        format!("chapter:{bid}:{cid}"),
                    )
                }
                (Some(text), None) => (text.to_owned(), 0, format!("draft:{bid}")),
                (None, Some(cid)) => {
                    let body = chapter(&mut tx, bid, cid).await?;
                    (
                        body["textContent"].as_str().unwrap_or("").to_owned(),
                        body["localVersion"].as_i64().unwrap_or(0),
                        format!("chapter:{bid}:{cid}"),
                    )
                }
                (None, None) => return Err("请提供正文或章节".into()),
            };
            crate::agent_quality::check(
                &mut tx,
                &crate::agent_quality::CheckRequest {
                    book_id: bid,
                    text: &text,
                    chapter_id,
                    artifact_id: &artifact_id,
                    reviewed_revision: revision,
                    ruleset_id: args["rulesetId"].as_str(),
                    profile_id: args["profileId"].as_str(),
                    target_words: args["targetWords"].as_u64().map(|v| v as usize),
                },
            )
            .await?
        }
        "index_control" => {
            crate::agent_indexer::set_index_paused(
                &mut tx,
                str_field(args, "bookId")?,
                args["paused"] == true,
            )
            .await?
        }
        "ruleset_preview" => {
            let bid = str_field(args, "bookId")?;
            // 会话内预览仍校验作品在引用范围内；记忆页按作品预览时不依附会话，不带作者原话
            let s = match args["sessionId"].as_str().filter(|v| !v.is_empty()) {
                Some(sid) => {
                    let s = session(&mut tx, sid).await?;
                    allowed_book(&mut tx, &s, bid, false).await?;
                    s
                }
                None => {
                    book(&mut tx, bid).await?;
                    json!({})
                }
            };
            crate::agent_ruleset::resolve(
                &mut tx,
                &s,
                &crate::agent_ruleset::ResolveRequest {
                    style_skill_id: args["styleSkillId"].as_str(),
                    book_id: bid,
                    skill_id: args["skillId"].as_str().filter(|v| !v.is_empty()),
                    author_instruction: args["authorInstruction"].as_str().filter(|v| !v.is_empty()),
                    persist: false,
                },
            )
            .await?
        }
        "canon_query" => {
            use crate::agent_canon as canon;
            let bid = str_field(args, "bookId")?;
            match str_field(args, "view")? {
                "cards" => json!(canon::refresh_cards(&mut tx, bid).await?),
                "threads" => json!(canon::list_threads(&mut tx, bid, args["status"].as_str()).await?),
                "suspects" => json!(canon::list_suspects(&mut tx, bid).await?),
                "facts" => json!(canon::list_facts(&mut tx, bid).await?),
                "rules" => json!(canon::list_rules(&mut tx, Some(bid)).await?),
                "profiles" => json!({"current":canon::book_profile_id(&mut tx,bid).await?,"profiles":canon::platform_profiles(&mut tx).await?}),
                "state" => {
                    canon::state_as_of(
                        &mut tx,
                        bid,
                        str_field(args, "characterId")?,
                        str_field(args, "chapterId")?,
                    )
                    .await?
                }
                "progress" => canon::index_progress(&mut tx, bid).await?,
                "story" => crate::agent_story::story_view(&mut tx, bid).await?,
                "notes" => json!(crate::agent_story::notes_view(&mut tx, bid).await?),
                "search" => crate::agent_story::search_view(&mut tx, bid, args["query"].as_str().unwrap_or(""),
                    args["limit"].as_i64().unwrap_or(8), args["untilChapterId"].as_str().filter(|v| !v.is_empty()), args["mode"] == "semantic").await?,
                "states" => json!(canon::list_states(&mut tx, bid).await?),
                "ledger" => canon::ledger(&mut tx, bid, args["chapterId"].as_str().filter(|v| !v.is_empty())).await?,
                "style" => json!({
                    "style":get(&mut tx, canon::CANON_NS, &format!("style:{bid}")).await?,
                    "lock":get(&mut tx, canon::AUTHOR_NS, &format!("style-lock:{bid}")).await?,
                    "samples":canon::confirmed_samples(&mut tx, bid).await?,
                    "aiSpans":canon::ai_span_counts(&mut tx, bid).await?,
                    "minSampleChars":crate::agent_style::MIN_SAMPLE_CHARS
                }),
                _ => return Err("未知的状态层视图".into()),
            }
        }
        "breakdown_profile" => crate::agent_breakdown::profile(&mut tx, &crate::agent_breakdown::project_id_arg(args)?).await?,
        "attach_cover" => crate::agent_packaging::attach_cover(&mut tx, str_field(args, "id")?, str_field(args, "coverUrl")?).await?,
        "breakdown_compare" => {
            crate::agent_breakdown::compare(&mut tx, str_field(args, "bookId")?, &crate::agent_breakdown::project_id_arg(args)?).await?
        }
        "delete_recipe" => {
            let key = str_field(args, "id")?;
            let mut recipe = get(&mut tx, crate::agent_canon::AUTHOR_NS, key).await?.ok_or("配方不存在")?;
            recipe["deletedAt"] = json!(now());
            put(&mut tx, crate::agent_canon::AUTHOR_NS, key, &recipe).await?;
            json!(true)
        }
        "adopt_book_plan" => {
            book_plan::adopt(
                &mut tx,
                str_field(args, "id")?,
                args["revision"].as_u64().ok_or("缺少草案版本")?,
            )
            .await?
        }
        "set_skill_enabled" => {
            let skill_id = str_field(args,"id")?;
            let mut record = get(&mut tx,NS,&format!("skill:{skill_id}")).await?.ok_or("技能不存在")?;
            if record["deletedAt"].is_number() { return Err("技能已删除".into()); }
            record["enabled"] = json!(args["enabled"].as_bool().ok_or("启停状态无效")?);
            put(&mut tx,NS,&format!("skill:{skill_id}"),&record).await?;
            record
        }
        "delete_memory" => {
            let prefix = "memory";
            let key = format!("{prefix}:{}", str_field(args, "id")?);
            let mut v = get(&mut tx, NS, &key).await?.ok_or("记录不存在")?;
            v["deletedAt"] = json!(now());
            v["status"] = json!("archived");
            v["enabled"] = json!(false);
            v["revision"] = json!(v["revision"].as_u64().unwrap_or(0) + 1);
            v["changedByRunId"] = Value::Null;
            memory::store(&mut tx, &v).await?;
            v
        }
        "save_artifact" => {
            let key = format!("artifact:{}", str_field(args, "id")?);
            let mut v = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            let before_edit = v.clone();
            if v["applied"] == true {
                return Err("已应用的成果不能直接改写，请在书籍中编辑".into());
            }
            if args["revision"].as_u64().unwrap_or(0) != v["revision"].as_u64().unwrap_or(0) {
                return Err("成果已更新，本次编辑保留，请重新读取后处理".into());
            }
            if v["kind"] == "skill" {
                crate::agent_skill_authoring::update(&mut v, &args["skillDraft"])?;
            } else if v["kind"] == "book_plan" {
                crate::agent_contract::validate_book_plan(&args["bookPlan"])?;
                v["bookPlan"] = book_plan::normalize(&args["bookPlan"], v["bookId"].as_str().is_some_and(|id| !id.is_empty()));
                v["title"] = json!("开书方案");
                v["content"] = json!(book_plan::content(&v["bookPlan"]));
                v["progress"] = book_plan::progress(&v["bookPlan"], false, v["bookId"].as_str().is_some_and(|b| !b.is_empty()));
            } else if v["kind"] == "material" {
                let kind = str_field(&v, "materialKind")?;
                creation::validate_fields(kind, &args["materialFields"])?;
                let content = creation::material_content(kind, &args["materialFields"]);
                v["materialFields"] = args["materialFields"].clone();
                v["content"] = json!(content);
            } else {
                v["content"] = json!(args["content"].as_str().ok_or("缺少成果内容")?);
            }
            v["revision"] = json!(v["revision"].as_u64().unwrap_or(0) + 1);
            crate::agent_feedback::mark_author_edit(&before_edit, &mut v);
            // An explicit author edit owns the new baseline; an interrupted
            // automatic revision must never restore over that edit.
            v["pendingRevision"]=Value::Null;
            v["readerReadout"]=Value::Null;
            if crate::agent_quality::is_prose(&v) {v["partial"]=json!(false);}
            for report in v["reports"].as_array_mut().into_iter().flatten() { report["stale"] = json!(true); }
            v["qualityWaiver"] = Value::Null;
            put(&mut tx, NS, &key, &v).await?;
            v
        }
        _ => return Err("未知 Agent 操作".into()),
    };
    tx.commit().await.map_err(err)?;
    Ok(value)
}

pub async fn active_project(db: &mut SqliteConnection, pid: &str) -> Result<Value, String> {
    let p = get(db, NS, &format!("project:{pid}"))
        .await?
        .ok_or("项目不存在")?;
    if p["deletedAt"].is_number() {
        return Err("项目已删除".into());
    }
    Ok(p)
}

/// None 是正常的独立会话，不补建隐形项目，不把任何一本参考书当成主作品。
pub async fn session_project(db: &mut SqliteConnection, s: &Value) -> Result<Value, String> {
    match s["projectId"].as_str().filter(|id| !id.is_empty()) {
        Some(pid) => active_project(db, pid).await,
        None => Ok(Value::Null),
    }
}

pub(crate) async fn create_book(db: &mut SqliteConnection, title: &str) -> Result<String, String> {
    if title.trim().is_empty() || title.chars().count() > 120 {
        return Err("书名应为 1—120 字".into());
    }
    let smallest:Option<i64>=sqlx::query_scalar("SELECT MIN(id) FROM (SELECT id FROM local_books UNION ALL SELECT id FROM local_volumes UNION ALL SELECT id FROM local_chapters)")
        .fetch_one(&mut *db).await.map_err(err)?;
    let bid = (-now() * 1000 - 900).min(smallest.unwrap_or(0) - 1);
    let vid = bid - 1;
    let cid = bid - 2;
    let stamp = chrono::Utc::now().to_rfc3339();
    let book_id = bid.to_string();
    let b = json!({"id":bid,"title":title,"intro":"","coverUrl":"","category":"","tags":[],
        "status":0,"wordCount":0,"textWordCount":0,"chapterCount":1,"authorId":"guest","createTime":stamp,
        "updateTime":stamp,"localOnly":true,"mergeStatus":"local","deletedAt":null});
    sqlx::query("INSERT INTO local_books(id,payload,title,groupId,mergeStatus,deletedAt,updateTime) VALUES(?,?,?,NULL,'local',NULL,?)")
        .bind(bid).bind(b.to_string()).bind(title).bind(&stamp).execute(&mut *db).await.map_err(err)?;
    let v = json!({"id":vid,"bookId":book_id,"title":"第一卷","summary":"","sortNo":1,"type":"volume","createTime":stamp,"updateTime":stamp,"deletedAt":null});
    sqlx::query(
        "INSERT INTO local_volumes(id,bookId,payload,deletedAt,sortNo) VALUES(?,?,?,NULL,1)",
    )
    .bind(vid)
    .bind(&book_id)
    .bind(v.to_string())
    .execute(&mut *db)
    .await
    .map_err(err)?;
    let c = json!({"id":cid,"bookId":book_id,"volumeId":vid.to_string(),"title":"第1章","summary":"","sortNo":1,"type":"chapter","wordCount":0,"textWordCount":0,"createTime":stamp,"updateTime":stamp,"deletedAt":null});
    sqlx::query("INSERT INTO local_chapters(id,bookId,volumeId,payload,deletedAt,sortNo) VALUES(?,?,?,?,NULL,1)")
        .bind(cid).bind(&book_id).bind(vid.to_string()).bind(c.to_string()).execute(db).await.map_err(err)?;
    Ok(book_id)
}

/// 一本书的卷（按目录顺序）。
pub async fn volumes(db: &mut SqliteConnection, book_id: &str) -> Result<Vec<Value>, String> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT payload FROM local_volumes WHERE bookId=? AND deletedAt IS NULL ORDER BY sortNo,id")
        .bind(book_id)
        .fetch_all(&mut *db)
        .await
        .map_err(err)?;
    rows.into_iter().map(|raw| serde_json::from_str(&raw).map_err(err)).collect()
}

/// 交给模型的引用：卷带上章节目录，人物与设定带上当前资料；选段与附件原样。存进会话的引用不变。
pub async fn expand_references(db: &mut SqliteConnection, refs: &Value) -> Result<Value, String> {
    let mut out = Vec::new();
    for r in refs.as_array().into_iter().flatten() {
        let mut r = r.clone();
        let bid = r["bookId"].as_str().unwrap_or("").to_owned();
        match r["kind"].as_str().unwrap_or("") {
            "volume" => {
                let vid = r["volumeId"].as_str().unwrap_or("").to_owned();
                let titles = catalog(db, &bid)
                    .await?
                    .iter()
                    .filter(|c| c["volumeId"].to_string().trim_matches('"') == vid)
                    .map(|c| json!({"chapterId":c["id"].to_string(),"title":c["title"]}))
                    .collect::<Vec<_>>();
                r["chapters"] = json!(titles);
            }
            "entity" => {
                let doc = crate::agent_creation::materials(db, &bid).await?;
                let entity_type = r["entityType"].as_str().unwrap_or("character").to_owned();
                let collection = crate::agent_creation::collection(&entity_type)?;
                let eid = r["entityId"].as_str().unwrap_or("").to_owned();
                match doc[collection].as_array().into_iter().flatten().find(|e| e["id"].to_string().trim_matches('"') == eid) {
                    Some(e) => r["content"] = json!(crate::agent_creation::material_content(&entity_type, e)),
                    None => r["missing"] = json!("这条资料已被删除"),
                }
            }
            // 引用的参考书：告诉模型这是什么、拆了多少，怎么用由作者这次的话决定
            "breakdown" => {
                if let Some(pid) = crate::agent_breakdown::project_of(&bid) {
                    match crate::agent_breakdown::project(db, pid).await {
                        Ok(p) => {
                            r["chapters"] = json!(p["chapters"].as_array().map_or(0, |a| a.len()));
                            r["words"] = p["wordCount"].clone();
                            r["indexedChapters"] = json!(crate::agent_breakdown::active_card_chapters(db, &bid).await?.len());
                            if p["importNotes"].as_array().is_some_and(|n| !n.is_empty()) {
                                r["importNotes"] = p["importNotes"].clone();
                            }
                            r["note"] = json!("作者放进来的整本书，按章存在本机，只读。导入不等于拆书：按作者这次的要求用 list_chapters、read_chapter 读原文，search_book 搜原文；要看大量章节的节奏、钩子、伏笔统计才 start_breakdown（章数多时会先向作者确认用量）。作者没说要拿它做什么时先问清楚。作者说这是自己的书稿、要续写或修改时，说明需要先在作品库导入为自己的作品。");
                        }
                        Err(_) => r["missing"] = json!("这本参考书已被删除"),
                    }
                }
            }
            // 上传过的资料：短的整篇给，长的只给开头，其余由 read_material 按段读
            "material" if r["materialId"].is_string() => {
                match get(db, OP_NS, &material_key(r["materialId"].as_str().unwrap_or(""))).await? {
                    Some(m) => {
                        let content = m["content"].as_str().unwrap_or("");
                        let chars = content.chars().count();
                        r["totalChars"] = json!(chars);
                        if chars <= MATERIAL_INLINE_CHARS {
                            r["content"] = json!(content);
                        } else {
                            r["content"] = json!(content.chars().take(MATERIAL_PREVIEW_CHARS).collect::<String>());
                            r["truncated"] = json!(true);
                            r["note"] = json!(format!("全文 {chars} 字，这里只有开头 {MATERIAL_PREVIEW_CHARS} 字；需要后文时用 read_material 按段读取"));
                        }
                    }
                    None => r["missing"] = json!("这份资料已不存在"),
                }
            }
            _ => {}
        }
        out.push(r);
    }
    Ok(json!(out))
}

/// 上传资料的上限：整本书走参考书导入，这里只收单篇资料。
pub const MATERIAL_MAX_CHARS: usize = 300_000;
/// 不超过这个字数的资料整篇交给模型；更长的只给开头。
pub const MATERIAL_INLINE_CHARS: usize = 12_000;
const MATERIAL_PREVIEW_CHARS: usize = 3_000;
const MATERIAL_READ_MAX: usize = 8_000;

pub fn material_key(material_id: &str) -> String {
    format!("material:{material_id}")
}

/// 输入框添加的本地资料：正文只存一份，会话与消息里的引用只带编号、标题和字数。
pub async fn put_material(db: &mut SqliteConnection, title: &str, content: &str) -> Result<Value, String> {
    let chars = content.chars().count();
    if content.trim().is_empty() {
        return Err("资料是空的".into());
    }
    if chars > MATERIAL_MAX_CHARS {
        return Err(format!("资料超过 {} 万字；整本小说请作为参考书导入", MATERIAL_MAX_CHARS / 10_000));
    }
    let material_id = id();
    let title = title.trim().chars().take(120).collect::<String>();
    put(db, OP_NS, &material_key(&material_id), &json!({"id":material_id,"title":title,"content":content,"chars":chars,"createdAt":now()})).await?;
    Ok(json!({"materialId":material_id,"title":title,"chars":chars}))
}

/// 按段读取本会话引用的资料（按字计偏移）。
pub fn referenced_material(session: &Value, material_id: &str) -> bool {
    let matches=|r:&Value|r["kind"]=="material" && r["materialId"]==material_id;
    session["references"].as_array().into_iter().flatten().any(matches)
        || session["messages"].as_array().into_iter().flatten().filter(|m|m["role"]=="user")
            .flat_map(|m|m["references"].as_array().into_iter().flatten()).any(matches)
}

async fn read_material(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    let material_id = str_field(args, "material_id")?;
    if !referenced_material(s,material_id) {
        return Err("这份资料不在本会话的引用里".into());
    }
    let m = get(db, OP_NS, &material_key(material_id)).await?.ok_or("这份资料已不存在")?;
    let text: Vec<char> = m["content"].as_str().unwrap_or("").chars().collect();
    let offset = (args["offset"].as_u64().unwrap_or(0) as usize).min(text.len());
    let limit = (args["limit"].as_u64().unwrap_or(6_000) as usize).clamp(500, MATERIAL_READ_MAX);
    let end = (offset + limit).min(text.len());
    Ok(json!({"materialId":material_id,"title":m["title"],"offset":offset,"totalChars":text.len(),
        "text":text[offset..end].iter().collect::<String>(),"nextOffset":if end < text.len() { json!(end) } else { Value::Null }}))
}

/// 不超过这么多章的拆书直接开始，不再弹确认。
const BREAKDOWN_AUTO_START: u64 = 10;

/// 模型发起拆书：章数多时由宿主生成确认卡（预估 token 与耗时），按作者点选执行。
/// 与榜单联网确认同一协议：第一次调用返回 confirmation_required，作者点选后同一调用再来一次，宿主据点选结果执行。
async fn start_breakdown_tool(db: &mut SqliteConnection, s: &Value, session_id: &str, args: &Value) -> Result<Value, String> {
    use crate::agent_breakdown_job as job;
    let pid = crate::agent_breakdown::project_id_arg(args)?;
    let from = args["from_chapter"].as_i64().unwrap_or(1);
    let to = args["to_chapter"].as_i64().unwrap_or(0);
    let estimate = job::estimate(db, &pid, from, to).await?;
    let (from, to) = (estimate["from"].as_i64().unwrap_or(1), estimate["to"].as_i64().unwrap_or(1));
    let missing = estimate["estimate"]["chapters"].as_u64().unwrap_or(0);
    let title = estimate["title"].as_str().unwrap_or("参考书").to_owned();
    if missing == 0 {
        return Ok(json!({"status":"already_indexed","message":format!("《{title}》第 {from}–{to} 章都已拆完，可以直接读画像"),
            "job":job::view(db, &pid).await?}));
    }
    let scope = json!({"projectId":pid,"from":from,"to":to});
    let key = format!("breakdown-consent:{session_id}:{pid}");
    let record = get(db, OP_NS, &key).await?.filter(|r| r["scope"] == scope && r["decision"].is_null());
    let decision = if missing <= BREAKDOWN_AUTO_START {
        Some(("full".to_owned(), String::new()))
    } else {
        record.as_ref().and_then(|r| {
            crate::agent_prompt::confirmation_answer(s, r["token"].as_str().unwrap_or("")).map(|(picked, reply)| {
                let option = picked
                    .and_then(|id| id.parse::<usize>().ok())
                    .and_then(|i| r["options"].get(i).and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| "unanswered".to_owned());
                (option, reply)
            })
        })
    };
    let Some((choice, reply)) = decision else {
        let tokens = estimate["estimate"]["totalTokens"].as_u64().unwrap_or(0);
        let minutes = &estimate["estimate"]["minutes"];
        let token = id();
        let question = format!("拆《{title}》第 {from}–{to} 章：其中 {missing} 章还没拆，预计用约 {} 万 token，耗时约 {}（在后台逐章进行，可以随时暂停，拆完我会接着把分析做完）。现在开始吗？",
            (tokens as f64 / 10_000.0).ceil() as u64, minutes_label(minutes));
        let mut choices = vec![format!("开始拆这 {missing} 章")];
        let mut options = vec!["full"];
        if to - from + 1 > BREAKDOWN_AUTO_START as i64 {
            choices.push(format!("先只拆第 {from}–{} 章看看", from + BREAKDOWN_AUTO_START as i64 - 1));
            options.push("opening");
        }
        choices.push("取消".to_owned());
        options.push("cancel");
        put(db, OP_NS, &key, &json!({"token":token,"scope":scope,"options":options,"issuedAt":now()})).await?;
        return Ok(json!({"status":"confirmation_required","confirmation":{"token":token,"question":question,"choices":choices}}));
    };
    if let Some(mut r) = record {
        r["decision"] = json!(choice);
        r["decidedAt"] = json!(now());
        put(db, OP_NS, &key, &r).await?;
    }
    let to = match choice.as_str() {
        "full" => to,
        "opening" => from + BREAKDOWN_AUTO_START as i64 - 1,
        "cancel" => return Ok(json!({"status":"declined","message":"作者取消了这次拆书，没有开始"})),
        _ => return Ok(json!({"status":"unanswered","authorReply":reply,"message":"作者没有点选，按作者的原话继续"})),
    };
    let started = job::start(db, &pid, from, to, Some(session_id), true).await?;
    Ok(json!({"status":"started","job":started,
        "note":"已在后台逐章拆解，对话里有进度卡，作者可以暂停或取消。本轮读不到新结果，告诉作者拆完后会自动接着分析，然后结束这一轮；不要反复查进度"}))
}

/// 预估耗时的说法：“5–12 分钟”“40 分钟–2 小时”“6–14 小时”。
pub fn minutes_label(minutes: &Value) -> String {
    let low = minutes[0].as_u64().unwrap_or(1).max(1);
    let high = minutes[1].as_u64().unwrap_or(low).max(low);
    let hours = |m: u64| {
        let h = (m as f64 / 30.0).round() / 2.0;
        if h.fract() == 0.0 { format!("{}", h as u64) } else { format!("{h:.1}") }
    };
    if high < 90 {
        if low == high { format!("{low} 分钟") } else { format!("{low}–{high} 分钟") }
    } else if low < 60 {
        format!("{low} 分钟–{} 小时", hours(high))
    } else {
        format!("{}–{} 小时", hours(low), hours(high))
    }
}

pub async fn validate_references(db: &mut SqliteConnection, refs: &Value) -> Result<(), String> {
    let refs = refs.as_array().ok_or("引用格式无效")?;
    if refs.len() > 30 {
        return Err("单次最多引用 30 项资料".into());
    }
    for r in refs {
        if r["kind"] == "material" && r["materialId"].is_string() {
            if get(db, OP_NS, &material_key(r["materialId"].as_str().unwrap_or(""))).await?.is_none() {
                return Err(format!("资料「{}」已不存在，请重新添加", r["title"].as_str().unwrap_or("")));
            }
        } else if r["kind"] == "material" {
            // 旧版直接把正文放在引用里
            if r["content"].as_str().unwrap_or("").len() > 160_000 {
                return Err("附件内容过长".into());
            }
        } else if r["kind"] == "breakdown" {
            let pid = str_field(r, "bookId").ok().and_then(crate::agent_breakdown::project_of).ok_or("拆书引用无效")?;
            crate::agent_breakdown::project(db, pid).await?;
        } else if r["kind"] == "rank" {
            let (sid, date) = (r["sourceId"].as_i64().unwrap_or(0), r["statDate"].as_str().unwrap_or(""));
            if !crate::agent_rank::snapshot_exists(db, sid, date).await? {
                return Err("引用的榜单快照已不在本地，请重新选择".into());
            }
        } else {
            if r["kind"] == "selection" && r["content"].as_str().unwrap_or("").chars().count() > 20_000 {
                return Err("选中的文字过长，请分段引用".into());
            }
            book(db, str_field(r, "bookId")?).await?;
        }
    }
    Ok(())
}

/// `code` 为空时按角色解析默认模型。规划：agent_planner → text_assist；写作：agent_writer → agent_planner → text_assist
/// （正文质量跟着规划模型走）；评审、索引：各自偏好 → text_assist → agent_planner（调用多、要结构化输出，
/// 不该默认跟着思考型的规划模型）。与前端 `AI_MODEL_GROUPS` 的回退链一致。`role` 为空只看 text_assist。
pub async fn model_config(
    db: &mut SqliteConnection,
    code: &str,
    role: &str,
) -> Result<Value, String> {
    let raw = get(db, "app-settings", "ew-local-ai-models")
        .await?
        .ok_or("请先在模型管理中配置文本模型")?;
    let models: Value =
        serde_json::from_str(raw.as_str().ok_or("模型设置格式无效")?).map_err(err)?;
    let chain: Vec<&str> = match role {
        "agent_writer" => vec![role, "agent_planner", "text_assist"],
        "agent_critic" | "agent_indexer" => vec![role, "text_assist", "agent_planner"],
        "agent_planner" => vec!["agent_planner", "text_assist"],
        _ => vec!["text_assist"],
    };
    let preferences = &models["preferences"];
    let code = if code.is_empty() {
        chain
            .iter()
            .find_map(|key| preferences[*key].as_str().filter(|v| !v.is_empty()))
            .unwrap_or("")
    } else {
        code
    };
    let model = models["models"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["id"].to_string() == code))
        .ok_or("所选模型不存在")?;
    if model["status"] != 1 || model["scene"] != "text" {
        return Err("所选文本模型已停用".into());
    }
    // 本机部署（Ollama / LM Studio 等）不需要 Key，网关不带鉴权头；其他供应商缺 Key 先拦下并说清怎么补
    if model["provider"] != "local" && model["apiKey"].as_str().is_none_or(str::is_empty) {
        return Err("所选模型还没有填写 API Key，请在模型管理中补上".into());
    }
    Ok(model.clone())
}

/// Resolve internal writing calls exactly as the runtime role routing does.
async fn writing_model(db: &mut SqliteConnection, session: &Value) -> Option<Value> {
    let planner = model_config(db,session["modelCode"].as_str().unwrap_or(""),"agent_planner").await.ok()?;
    role_model(db,"agent_writer",&planner).await.ok()
}

fn writing_model_identity(model: Option<&Value>) -> Value {
    model.map(|m| {
        let configuration=json!({"provider":m["provider"],"modelCode":m["modelCode"],"thinking":m["thinking"],
            "maxContext":m["maxContext"],"maxOutputTokens":m["maxOutputTokens"]});
        json!({"id":m["id"],"name":m["name"],"configuration":configuration,
            "configurationFingerprint":creation::version(&configuration)})
    }).unwrap_or(Value::Null)
}

/// 评审、索引角色本轮用的模型：自己的偏好 → 写作助手（text_assist）→ 本轮规划模型。
/// 作者在输入框指定的模型只决定规划角色；这两个角色调用多、要结构化输出，默认不跟思考型的规划模型。
pub async fn support_model(
    db: &mut SqliteConnection,
    role: &str,
    planner: &Value,
) -> Result<Value, String> {
    Ok(model_config(db, "", role).await.unwrap_or_else(|_| planner.clone()))
}

/// 某角色单独设置了默认模型就用它，否则沿用本轮已确定的模型（作者在输入框指定的，或规划角色解析出的）。
/// 作者的显式选择只决定规划角色，不覆盖其他角色各自的偏好。
pub async fn role_model(
    db: &mut SqliteConnection,
    role: &str,
    fallback: &Value,
) -> Result<Value, String> {
    let code = get(db, "app-settings", "ew-local-ai-models")
        .await?
        .and_then(|raw| raw.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()))
        .and_then(|models| models["preferences"][role].as_str().map(str::to_owned))
        .unwrap_or_default();
    if code.is_empty() {
        return Ok(fallback.clone());
    }
    Ok(model_config(db, &code, role)
        .await
        .unwrap_or_else(|_| fallback.clone()))
}

pub async fn allowed_book(
    db: &mut SqliteConnection,
    s: &Value,
    bid: &str,
    write: bool,
) -> Result<(), String> {
    let p = session_project(db, s).await?;
    let related = p["bookId"].as_str() == Some(bid);
    let target = s["targetBookId"].as_str() == Some(bid);
    let reference = s["references"].as_array().is_some_and(|rs| {
        rs.iter()
            .any(|r| r["kind"] != "material" && r["bookId"] == bid)
    });
    // 本会话以前引用过、任务做完后撤下的书：仍可只读（见 agent_host::settle_references）
    let remembered = !write && read_source(s, bid);
    if (write && !target) || (!write && !related && !target && !reference && !remembered) {
        return Err(if write {
            "请先根据作者指令确定本次写入目标，引用资料不会自动授权修改"
        } else {
            "该作品不在本会话引用或关联范围内"
        }
        .into());
    }
    book(db, bid).await?;
    Ok(())
}

/// 本会话读过的书与参考书（引用撤下后记在 readSources 里），后续只读仍放行
pub fn read_source(s: &Value, bid: &str) -> bool {
    s["readSources"].as_array().is_some_and(|v| v.iter().any(|x| x == bid))
}

/// 读原文、检索用的范围检查：除了作者作品，也接受本会话引用过的参考书（拆书库，只读）。
/// 写作类工具仍用 `allowed_book`，参考书进不去。
pub async fn allowed_source(db: &mut SqliteConnection, s: &Value, bid: &str) -> Result<(), String> {
    let Some(pid) = crate::agent_breakdown::project_of(bid) else {
        return allowed_book(db, s, bid, false).await;
    };
    let referenced = s["references"].as_array().is_some_and(|rs| rs.iter().any(|r| r["kind"] == "breakdown" && r["bookId"] == bid));
    if !referenced && !read_source(s, bid) {
        return Err("这本参考书不在本会话的引用里".into());
    }
    crate::agent_breakdown::project(db, pid).await?;
    Ok(())
}

pub async fn tool(
    root: &Path,
    session_id: &str,
    run_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    tool_with_cancellation(root, session_id, run_id, name, args,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).await
}

pub async fn tool_with_cancellation(
    root: &Path, session_id: &str, run_id: &str, name: &str, args: &Value,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value,String> {
    let value=crate::agent_tool_boundary::public_result(name,dispatch_tool(root,session_id,run_id,name,args,cancelled).await?);
    crate::agent_contract::validate_result(name,&value)?;
    Ok(value)
}

async fn dispatch_tool(
    root: &Path,
    session_id: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value, String> {
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("任务已停止".into());
    }
    let mut normalized = crate::agent_contract::canonical_arguments(args);
    let args = &normalized;
    if let Some(error) = crate::agent_contract::tool_error(name, args)? {
        return Ok(error);
    }
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let s = session(&mut db, session_id).await?;
    if s["runId"] != run_id || s["status"] != "running" {
        return Err("任务已失效或停止".into());
    }
    if let Some(error) = crate::agent_tool_boundary::prepare(&mut db, &s, name, &mut normalized).await? {
        return Ok(error);
    }
    let args = &normalized;
    if [
        "save_chapter",
        "save_artifact",
        "save_material",
        "create_volume",
        "create_chapter",
        "apply_creation_artifact",
        "record_book_memory",
        "prepare_book_plan",
        "adopt_book_plan",
        "book_plan_adopt_begin",
        "book_plan_adopt_finish",
        "prepare_skill",
        "draft_begin",
        "short_begin",
        "open_chapter_candidate",
        "plan_short_story",
        "artifact_replace",
        "delivery_begin",
        "delivery_progress",
        "delivery_restore",
        "configure_book_writing",
        "plan_scenes",
        "plan_chapters",
        "save_selection_cards",
        "save_rule",
        "record_suspect_verdict",
        "save_breakdown_recipe",
        "save_comparison",
        "start_breakdown",
        "prepare_distillation",
        "save_name_candidates",
        "save_book_intro",
        "request_cover",
        "update_volume",
        "update_chapter",
        "update_book",
        "delete_volume",
        "delete_chapter",
        "delete_book",
    ]
    .contains(&name)
        && s["pendingInputs"].as_array().is_some_and(|v| !v.is_empty())
    {
        return Ok(crate::agent_tool_boundary::failure("author_input_pending", crate::agent_tool_boundary::FailureKind::Precondition,
            "作者已补充要求，请先读取补充要求并重新规划，不沿用旧修改。", true, "", json!({})));
    }
    if let Some(job_id) = s["batchJobId"].as_str() {
        let chapter_action = matches!(name,"draft_begin" | "plan_scenes" | "save_chapter")
            || name == "save_artifact" && args["kind"] == "chapter";
        if chapter_action {
            let job = crate::agent_batch::load(&mut db,job_id).await?;
            let index = job["cursor"].as_u64().unwrap_or(0) as usize;
            if args["book_id"] != job["bookId"] || args["chapter_id"] != job["queue"][index]["chapterId"] {
                return Ok(json!({"ok":false,"toolError":{"code":"batch_target_mismatch","retryable":true,
                    "message":"这次批量步骤只处理当前排队的章节，不能修改其他章节"}}));
            }
            if name == "save_chapter" && args["artifact_id"].as_str().unwrap_or("").is_empty() {
                return Ok(json!({"ok":false,"toolError":{"code":"batch_candidate_required","retryable":true,
                    "message":"批量正文必须先形成可追踪的章节候选，再按成果编号保存"}}));
            }
        }
    }
    let formal = [
        "select_write_target",
        "save_chapter",
        "create_volume",
        "create_chapter",
        "apply_creation_artifact",
        "update_volume",
        "update_chapter",
        "update_book",
    ]
    .contains(&name)
        || (name == "save_material" && args["draft"] == false);
    if formal {
        if let Some(job_id) = s["batchJobId"].as_str() {
            let job = crate::agent_batch::load(&mut db, job_id).await?;
            if job["mode"] != "write" {
                return Ok(json!({"ok":false,"toolError":{"code":"batch_read_only","retryable":false,
                    "message":"本批次只生成候选，不能通过模型工具写入正式作品；作者可在成果面板采用"}}));
            }
        }
        let candidate = if name == "apply_creation_artifact" {
            get(
                &mut db,
                NS,
                &format!("artifact:{}", str_field(args, "artifact_id")?),
            )
            .await?
        } else {
            None
        };
        let bid = args["book_id"]
            .as_str()
            .or_else(|| candidate.as_ref().and_then(|v| v["bookId"].as_str()))
            .unwrap_or("");
        if !bid.is_empty() && book_plan::needs_review(&mut db, bid).await? {
            return Ok(book_plan::review_required());
        }
        if name != "select_write_target" && !bid.is_empty() && s["targetBookId"] != bid {
            // 可读作品不等于本任务可写。前置条件缺失交回工具循环处理，不能隐式授权或直接使任务崩溃。
            allowed_book(&mut db, &s, bid, false).await?;
            return Ok(
                json!({"ok":false,"toolError":{"code":"write_target_required","retryable":true,
                "message":"本次工具未执行：这件事的写入目标还不是这本书。作者已明确要求修改此作品时，先调用 select_write_target 再继续；若指令只要求分析或目标不明确，应继续讨论。不要仅凭引用就进行写入。"}}),
            );
        }
    }
    // 参考书只能借方法：本会话接触过的对标书人物名不能写进作者的开书草案与作品资料
    if matches!(name, "prepare_book_plan" | "save_material") {
        let names = crate::agent_breakdown::session_reference_names(&mut db, &s).await?;
        let written = if name == "prepare_book_plan" {
            args["plan"].to_string()
        } else {
            format!("{}\n{}", args["title"], args["fields"])
        };
        if let Some(error) = crate::agent_breakdown::leak_error(&written, &names) {
            return Ok(error);
        }
    }
    // 保存正文先核对依据（候选、版本、读取记录）：核对不过直接返回工具错误，不发一张注定失败的确认卡；
    // 候选比正文旧（作者在候选生成后改过这一章）是事实，交给审批门，任何入口都先问作者并写明会覆盖。
    let facts = if name == "save_chapter" {
        allowed_book(&mut db, &s, str_field(args, "book_id")?, true).await?;
        if let Some(replay) = save_chapter_receipt(&mut db, session_id, args).await? {
            return Ok(replay);
        }
        match chapter_save_basis(&mut db, session_id, run_id, args).await? {
            Ok(basis) => basis.facts(),
            Err(error) => return Ok(error),
        }
    } else if name == "apply_creation_artifact" {
        apply_basis(&mut db, str_field(args, "artifact_id")?).await?
    } else {
        if name == "create_automation" {
            if let Some(error) = crate::agent_scheduler::preflight(&mut db, &s, args).await? {
                return Ok(error);
            }
        }
        Value::Null
    };
    // 审批门：删除在任何入口都先问作者；要求逐次确认的入口（微信）对作品写入也先问。确认卡由宿主生成、按点选放行。
    if let Some(pending) = crate::agent_approval::gate(&mut db, &s, run_id, name, args, &facts).await? {
        return Ok(pending);
    }
    if let Some(error)=crate::agent_tool_boundary::bind_operation(&mut db,&s,name,args).await? { return Ok(error); }
    match name {
        "list_books" => crate::agent_library::list_books(&mut db, args["keyword"].as_str().unwrap_or(""), args["limit"].as_u64().unwrap_or(20) as usize).await,
        "open_book" => crate::agent_library::open_book(root, session_id, str_field(args, "book_id")?).await,
        "update_volume" | "update_chapter" => crate::agent_library::update_entry(root, session_id,
            if name == "update_volume" { "volume" } else { "chapter" }, name, args).await,
        "update_book" => crate::agent_library::update_book(root, session_id, args).await,
        "delete_volume" | "delete_chapter" => crate::agent_library::delete_entry(root, session_id,
            if name == "delete_volume" { "volume" } else { "chapter" }, name, args).await,
        "delete_book" => crate::agent_library::delete_book(root, session_id, args).await,
        "rank_trends" => crate::agent_rank::trends(&mut db, args["source_id"].as_i64().unwrap_or(0),
            args["stat_date"].as_str().unwrap_or(""), args["compare_date"].as_str().unwrap_or(""),
            args["limit"].as_u64().unwrap_or(20) as usize).await,
        "prepare_book_plan" => book_plan::save(root, session_id, args).await,
        "load_book_context" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let mut current = session(&mut tx, session_id).await?;
            current["contextBookId"] = json!(bid);
            save_session(&mut tx, &mut current).await?;
            let out = memory::context(&mut tx, &current, Some(bid)).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "record_book_memory" => memory::record(root, session_id, args).await,
        "work_note" => {
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_journal::record(&mut tx, &s, args).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "prepare_distillation" => {
            let pid = str_field(args,"project_id")?;
            let bid = crate::agent_breakdown::book_id(pid);
            allowed_source(&mut db,&s,&bid).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let mut out = crate::agent_distillation::prepare(&mut tx,&s,pid,args["focus"].as_str().unwrap_or(""),str_field(args,"operation_id")?).await?;
            tx.commit().await.map_err(err)?;
            if let Some(map)=out.as_object_mut(){map.remove("chapterSizes");}
            Ok(out)
        }
        "read_distillation" => {
            let jid=str_field(args,"distillation_id")?;
            crate::agent_distillation::scoped(&mut db,&s,jid).await?;
            if let Some(nid)=args["node_ref"].as_str().filter(|v|!v.is_empty()) {
                if args["evidence_ref"].as_str().is_some_and(|v|!v.is_empty()){return Err("节点与证据请分别读取".into());}
                return crate::agent_distillation::read_node(&mut db,jid,nid).await;
            }
            if let Some(eid)=args["evidence_ref"].as_str().filter(|v|!v.is_empty()) {return crate::agent_distillation::read_evidence(&mut db,jid,eid).await;}
            let mut out=crate::agent_distillation::read_results(&mut db,jid,args["offset"].as_u64().unwrap_or(0) as usize,args["limit"].as_u64().unwrap_or(10) as usize).await?;
            if let Some(map)=out["job"].as_object_mut(){map.remove("chapterSizes");}
            Ok(out)
        }
        "list_writing_profiles" => Ok(json!(crate::agent_canon::platform_profiles(&mut db).await?)),
        "configure_book_writing" => {
            let bid = str_field(args,"book_id")?;
            allowed_book(&mut db,&s,bid,false).await?;
            let quote = str_field(args,"author_quote")?.trim();
            if quote.is_empty() || !s["messages"].as_array().into_iter().flatten().any(|m|
                m["role"] == "user" && m["text"].as_str().is_some_and(|text|text.contains(quote))) {
                return Err("写作要求必须附带本会话作者的实际原话".into());
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            if let Some(id) = args["style_skill_id"].as_str().filter(|s| !s.is_empty()) {
                let style = crate::agent_ruleset::style_skill(&mut tx, id).await?;
                if style["toolError"].is_object() { return Ok(style); }
            }
            if let Some(profile) = args["profile_id"].as_str() { crate::agent_canon::set_book_profile(&mut tx,bid,profile).await?; }
            let key = format!("book-requirements:{bid}");
            let mut r = get(&mut tx,crate::agent_canon::AUTHOR_NS,&key).await?.unwrap_or(json!({"bookId":bid}));
            for (source,target) in [("audience","audience"),("style_requirements","styleRequirements"),("style_skill_id","styleSkillId")] {
                if let Some(value) = args[source].as_str() { r[target]=json!(value); }
            }
            r["authorQuote"]=json!(quote);
            put(&mut tx,crate::agent_canon::AUTHOR_NS,&key,&r).await?;
            let profile = crate::agent_canon::book_profile_id(&mut tx,bid).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"saved":true,"bookId":bid,"profileId":profile,"requirements":r}))
        }
        "prepare_prose_task" | "bind_prose_task" => {
            let mut tx=crate::desktop_storage::begin_write(&mut db).await?;
            let result=crate::agent_task::prepare(&mut tx,&s,args,name=="bind_prose_task").await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "delivery_begin" | "delivery_progress" | "delivery_restore" => {
            let mut tx=crate::desktop_storage::begin_write(&mut db).await?;
            let result=match name {
                "delivery_begin" => crate::agent_delivery::begin(&mut tx,&s,args).await?,
                "delivery_progress" => crate::agent_delivery::progress(&mut tx,&s,args).await?,
                _ => crate::agent_delivery::restore(&mut tx,&s,args).await?,
            };
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "check_chapter" => {
            // 检查会话成果（章节候选）或正式章节；成果版本不符时拒绝，报告写回成果的 reports 列表
            let artifact_id = args["artifact_id"].as_str().filter(|v| !v.is_empty());
            let chapter_id = args["chapter_id"].as_str().filter(|v| !v.is_empty());
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let report = match (artifact_id, chapter_id) {
                (Some(aid), _) => {
                    let key = format!("artifact:{aid}");
                    let mut artifact = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
                    if artifact["sessionId"] != session_id {
                        return Ok(json!({"ok":false,"toolError":{"code":"candidate_requires_copy","retryable":true,
                            "message":"该候选来自另一会话。作者要求改稿时，用 polish_draft 根据当前要求建立修订副本；不要直接改动原会话的候选。"}}));
                    }
                    let revision = artifact["revision"].as_i64().unwrap_or(0);
                    if args["expected_revision"].as_i64().is_some_and(|expected| expected != revision) {
                        return Ok(json!({"ok":false,"toolError":{"code":"stale_artifact","retryable":true,
                            "message":"成果已被作者编辑，请 read_artifact 取最新 revision 后再检查"}}));
                    }
                    if !crate::agent_quality::is_prose(&artifact) {
                        return Err("只能体检章节候选或短篇稿".into());
                    }
                    let short = artifact["kind"] == "short";
                    let bid = artifact["bookId"].as_str().unwrap_or("").to_owned();
                    if bid.is_empty() && !short {
                        return Err("成果未关联作品，无法按画像检查".into());
                    }
                    if !bid.is_empty() {
                        allowed_book(&mut tx, &s, &bid, false).await?;
                    }
                    let text = artifact["content"].as_str().unwrap_or("").to_owned();
                    let rules = crate::agent_ruleset::for_artifact(&mut tx, &s, &mut artifact, args["ruleset_id"].as_str()).await?;
                    if rules["toolError"].is_object() { return Ok(rules); }
                    crate::agent_writing_basis::ensure(&mut tx, &mut artifact).await?;
                    let ruleset_arg = artifact["rulesetId"].as_str().map(str::to_owned);
                    let mut report = crate::agent_quality::check_artifact(&mut tx, &artifact, &text, aid, revision, ruleset_arg.as_deref()).await?;
                    report["writingBasisFingerprint"] = artifact["writingBasis"]["fingerprint"].clone();
                    let mut reports = artifact["reports"].as_array().cloned().unwrap_or_default();
                    for old in &mut reports {
                        if old["reviewedFingerprint"] != report["reviewedFingerprint"] {
                            old["stale"] = json!(true);
                        }
                    }
                    reports.push(report.clone());
                    artifact["reports"] = json!(reports);
                    artifact["textFingerprint"] = report["reviewedFingerprint"].clone();
                    put(&mut tx, NS, &key, &artifact).await?;
                    // 读者评审需要的材料：拍子与规则集
                    let plan_text = match artifact["planArtifactId"].as_str().filter(|v| !v.is_empty()) {
                        Some(pid) => get(&mut tx, NS, &format!("artifact:{pid}"))
                            .await?
                            .and_then(|p| p["content"].as_str().map(str::to_owned))
                            .unwrap_or_default(),
                        None => String::new(),
                    };
                    let ruleset_id = args["ruleset_id"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .or_else(|| artifact["rulesetId"].as_str().map(str::to_owned));
                    // 短篇没有长篇的线索表，读者评审只看结构卡与规则
                    (report, if short { String::new() } else { bid }, text, plan_text, ruleset_id)
                }
                (None, Some(cid)) => {
                    let bid = str_field(args, "book_id")?;
                    allowed_book(&mut tx, &s, bid, false).await?;
                    let body = chapter(&mut tx, bid, cid).await?;
                    let text = body["textContent"].as_str().unwrap_or("").to_owned();
                    let report = crate::agent_quality::check(
                        &mut tx,
                        &crate::agent_quality::CheckRequest {
                            book_id: bid,
                            text: &text,
                            chapter_id: Some(cid),
                            artifact_id: &format!("chapter:{bid}:{cid}"),
                            reviewed_revision: body["localVersion"].as_i64().unwrap_or(0),
                            ruleset_id: args["ruleset_id"].as_str(),
                            profile_id: None,
                            target_words: None,
                        },
                    )
                    .await?;
                    (report, bid.to_owned(), text, String::new(), args["ruleset_id"].as_str().filter(|v| !v.is_empty()).map(str::to_owned))
                }
                (None, None) => return Err("请指定成果或章节".into()),
            };
            let (report, bid, text, plan_text, ruleset_id) = report;
            tx.commit().await.map_err(err)?;
            let mut out = report;
            out["summary"] = crate::agent_quality::summary(&out);
            // 附给 worker 的评审材料；不进入契约校验过的报告主体，只是同一次返回里的附加字段
            let profile_id = out["profileId"].as_str().unwrap_or(crate::agent_canon::DEFAULT_PROFILE).to_owned();
            let profile = crate::agent_canon::platform_profile(&mut db, &profile_id).await?;
            out["text"] = json!(text);
            out["persona"] = profile["readerPersona"].clone();
            out["plan_text"] = json!(plan_text);
            out["writing_basis"] = if let Some(aid) = artifact_id {
                get(&mut db,NS,&format!("artifact:{aid}")).await?.map(|a|a["writingBasis"].clone()).unwrap_or(Value::Null)
            } else if let Some(cid) = chapter_id.filter(|_|!bid.is_empty()) {
                crate::agent_writing_basis::for_chapter(&mut db,&bid,cid,ruleset_id.as_deref()).await?
            } else { Value::Null };
            out["protected_context"] = if let Some(aid) = artifact_id {
                let a = get(&mut db,NS,&format!("artifact:{aid}")).await?.ok_or("候选不存在")?;
                out["target_words"] = a["targetWords"].clone();
                out["revision_before"] = a["pendingRevision"]["before"].clone();
                out["revision_operation"] = a["pendingRevision"]["operationId"].clone();
                crate::agent_delivery::protected_context(&mut db,&a).await?
            } else { out["writing_basis"]["protectedContext"].clone() };
            if out["target_words"].is_null() {
                let words = &profile["chapterWords"];
                out["target_words"] = json!((words[0].as_u64().unwrap_or(2000) + words[1].as_u64().unwrap_or(4000)) / 2);
            }
            // 编辑只拿作者的要求；方法技能、画像与文风技能正文不进评审
            out["rules_text"] = json!(match ruleset_id {
                Some(r) => crate::agent_ruleset::load(&mut db, &r).await.map(|v| crate::agent_ruleset::writer_text(&v)).unwrap_or_default(),
                None => String::new(),
            });
            out["open_threads"] = if bid.is_empty() {
                json!([])
            } else {
                json!(crate::agent_canon::list_threads(&mut db, &bid, Some("open")).await?
                    .iter().map(|t| json!({"threadId":t["threadId"],"title":t["title"],"kind":t["kind"]})).collect::<Vec<_>>())
            };
            Ok(out)
        }
        "assemble_context" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            // 预算按本轮规划模型的上下文与输出上限折算；没有模型配置时用保守默认值
            let (max_context, reserve) = match model_config(&mut db, s["modelCode"].as_str().unwrap_or(""), "agent_planner").await {
                Ok(m) => (
                    m["maxContext"].as_u64().unwrap_or(32000) as usize,
                    m["maxOutputTokens"].as_u64().unwrap_or(4096) as usize,
                ),
                Err(_) => (32000, 4096),
            };
            let hints = args["characters_hint"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_context::assemble(
                &mut tx,
                Some(&s),
                &crate::agent_context::ContextRequest {
                    book_id: bid,
                    purpose: args["purpose"].as_str().unwrap_or("write"),
                    chapter_id: args["chapter_id"].as_str().filter(|v| !v.is_empty()),
                    ruleset_id: args["ruleset_id"].as_str().filter(|v| !v.is_empty()),
                    query_hint: args["query_hint"].as_str().filter(|v| !v.is_empty()),
                    characters_hint: hints,
                    model_max_context: max_context,
                    output_reserve: reserve,
                    budget_tokens: args["budget_tokens"].as_u64().filter(|v| *v > 0).map(|v| v as usize),
                },
            )
            .await?;
            if out["ok"] == true {
                put(&mut tx, OP_NS, &format!("candidate-context:{session_id}:{run_id}"), &out["candidateSources"]).await?;
            }
            tx.commit().await.map_err(err)?;
            let mut out = out;
            if out["ok"] == true {
                out["rendered"] = json!(crate::agent_context::render(&out));
                // 正文只在 rendered 里给一份；各层只留标签、来源与用量，起草时宿主会按同样的来源重新装配
                for blocks in out["layers"].as_object_mut().into_iter().flat_map(|layers| layers.values_mut()) {
                    for block in blocks.as_array_mut().into_iter().flatten() {
                        if let Some(fields) = block.as_object_mut() {
                            fields.remove("text");
                        }
                    }
                }
            }
            Ok(out)
        }
        "resolve_ruleset" => {
            // 不挂作品的短篇：按指定画像解析，只带作者全局规则
            let bid = match args["book_id"].as_str().filter(|v| !v.is_empty()) {
                Some(bid) => {
                    allowed_book(&mut db, &s, bid, false).await?;
                    bid.to_owned()
                }
                None => {
                    let profile_id = args["profile_id"].as_str().filter(|v| !v.is_empty()).unwrap_or(crate::agent_quality::SHORT_PROFILE);
                    if crate::agent_canon::platform_profile(&mut db, profile_id).await.is_err() {
                        return Ok(json!({"ok":false,"toolError":{"code":"unknown_profile","retryable":true,
                            "message":"没有这个平台画像；短篇用 zhihu 或 gongzhonghao，或作者自建画像的编号"}}));
                    }
                    crate::agent_canon::short_book(profile_id)
                }
            };
            let bid = bid.as_str();
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_ruleset::resolve(
                &mut tx,
                &s,
                &crate::agent_ruleset::ResolveRequest {
                    style_skill_id: args["style_skill_id"].as_str(),
                    book_id: bid,
                    skill_id: args["skill_id"].as_str().filter(|v| !v.is_empty()),
                    author_instruction: args["author_instruction"].as_str().filter(|v| !v.is_empty()),
                    persist: true,
                },
            )
            .await?;
            tx.commit().await.map_err(err)?;
            Ok(crate::agent_ruleset::tool_view(&out))
        }
        "list_threads" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let status = args["status"].as_str().filter(|v| !v.is_empty() && *v != "all");
            Ok(json!({"threads":crate::agent_canon::list_threads(&mut db, bid, status).await?}))
        }
        "update_thread" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let quote = args["author_quote"].as_str().unwrap_or("").trim();
            if quote.is_empty() || !author_said_any(&s, &[quote]) {
                return Ok(json!({"ok":false,"toolError":{"code":"author_quote_required","retryable":true,
                    "message":"改线索状态必须附作者在本任务里的原话摘录；作者没有明确决定时不要修改线索"}}));
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_canon::override_thread(
                &mut tx,
                bid,
                str_field(args, "thread_id")?,
                str_field(args, "status")?,
                args["note"].as_str().unwrap_or(""),
            )
            .await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "read_character_state" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            crate::agent_canon::state_as_of(
                &mut db,
                bid,
                str_field(args, "character_id")?,
                str_field(args, "as_of_chapter_id")?,
            )
            .await
        }
        "search_book" => {
            let bid = str_field(args, "book_id")?;
            allowed_source(&mut db, &s, bid).await?;
            let query = str_field(args, "query")?;
            let limit = args["limit"].as_i64().unwrap_or(8).clamp(1, 20);
            let scope = args["scope"].as_str().unwrap_or("all");
            let chunks = if scope == "entities" {
                Vec::new()
            } else {
                crate::agent_indexer::search_chunks(&mut db, bid, query, limit).await?
            };
            let entities = if scope == "text" {
                Vec::new()
            } else {
                crate::agent_indexer::search_entities(&mut db, bid, query, limit).await?
            };
            let search_pending = if let Some(pid)=crate::agent_breakdown::project_of(bid) {
                let p=crate::agent_breakdown::project(&mut db,pid).await?;
                p["searchCursor"].as_u64().is_some_and(|n|n<p["chapters"].as_array().map_or(0,Vec::len) as u64)
            } else { false };
            // 记忆（这本书的决定、摘要、候选与作者层偏好）与正文一起命中；参考书没有记忆
            let memories = if scope == "entities" || crate::agent_breakdown::project_of(bid).is_some() {
                Vec::new()
            } else {
                crate::agent_indexer::search_memories(&mut db, bid, query, limit).await?
            };
            Ok(json!({"chunks":chunks,"entities":entities,"memories":memories,"indexing":search_pending,
                "note":if search_pending { "全文检索仍在后台建立，当前命中可能不完整；可以按章读取原文，不要把零命中解释为书中没有" } else { "" }}))
        }
        "read_timeline" | "read_storylines" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let doc = creation::materials(&mut db, bid).await?;
            Ok(if name == "read_timeline" {
                creation::plot_timeline(&doc)
            } else {
                creation::plot_storylines(&doc)
            })
        }
        "read_style_profile" => {
            let bid = args["book_id"].as_str().unwrap_or("");
            if !bid.is_empty() { allowed_book(&mut db, &s, bid, false).await?; }
            let mut examples = crate::agent_style::examples(&mut db,bid,args["scene_query"].as_str().unwrap_or("")).await?;
            examples.truncate(crate::agent_style::EXAMPLE_CATALOG_LIMIT);
            let mut editing_examples = crate::agent_feedback::evidence(&mut db,bid).await?;
            editing_examples.truncate(3);
            let sample_count = crate::agent_canon::confirmed_samples(&mut db,bid).await?.len();
            let style = get(&mut db, crate::agent_canon::CANON_NS, &format!("style:{bid}")).await?.unwrap_or(json!({}));
            Ok(json!({"points":if style["locked"] == true {style["points"].clone()} else {json!("")},
                "sampleChapters":sample_count,"sampleChars":style["sampleChars"],"insufficient":style["insufficient"],"metrics":style["metrics"],"locked":style["locked"],"examples":examples,"editingExamples":editing_examples,
                "note":"不要自行推断作者文风。统计只描述样本，不构成作者要求。按场景用途选择最多两个 example id 写入 styleSampleIds；示例人物与事件不是本书事实。"}))
        }
        "propose_writing_preference" => {
            let bid=str_field(args,"book_id")?;
            allowed_book(&mut db,&s,bid,false).await?;
            let mut tx=crate::desktop_storage::begin_write(&mut db).await?;
            let result=crate::agent_feedback::propose(&mut tx,bid,args).await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "save_rule" | "record_suspect_verdict" => {
            let quote = args["author_quote"].as_str().unwrap_or("").trim();
            if quote.is_empty() || !author_said_any(&s, &[quote]) {
                return Ok(json!({"ok":false,"toolError":{"code":"author_quote_required","retryable":true,
                    "message":"必须附作者在本任务里的原话摘录；作者没有明确提出时不要保存规则或裁决疑点"}}));
            }
            let op = format!("{name}:{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(done) = get(&mut db, OP_NS, &op).await? {
                return Ok(done);
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = if name == "save_rule" {
                let scope = str_field(args, "scope")?;
                let bid = args["book_id"].as_str().filter(|v| !v.is_empty());
                if scope == "book" {
                    allowed_book(&mut tx, &s, bid.ok_or("本书规则必须指定作品")?, false).await?;
                }
                crate::agent_canon::save_rule(&mut tx, scope, bid, str_field(args, "category")?, str_field(args, "text")?, "作者对话").await?
            } else {
                let bid = str_field(args, "book_id")?;
                allowed_book(&mut tx, &s, bid, false).await?;
                crate::agent_canon::record_verdict(
                    &mut tx,
                    bid,
                    str_field(args, "issue_key")?,
                    str_field(args, "decision")?,
                    args["note"].as_str().unwrap_or(""),
                )
                .await?
            };
            put(&mut tx, OP_NS, &op, &out).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "impact_report" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            crate::agent_canon::impact_report(&mut db, bid, str_field(args, "chapter_id")?).await
        }
        // ---- 拆书 2.0：参考书是作者自己的拆书库，不受会话引用范围限制；写入作者作品的仍要校验 ----
        "list_skills" => Ok(json!(list(&mut db, NS,"skill:").await?.into_iter()
            .filter(|skill| !skill["deletedAt"].is_number() && !skill["retiredAt"].is_number())
            .map(|skill|json!({"id":skill["id"],"name":skill["name"],"description":skill["description"],"kind":skill["kind"],"source":skill["source"],"enabled":skill["enabled"]})).collect::<Vec<_>>())),
        "read_skill" => crate::agent_skill_authoring::read_manifest(&mut db,str_field(args,"skill_id")?).await,
        "read_skill_resource" => crate::agent_skill_authoring::read_resource(&mut db,str_field(args,"skill_id")?,str_field(args,"path")?,
            args["offset"].as_u64().unwrap_or(0) as usize,args["limit"].as_u64().unwrap_or(4000) as usize).await,
        "prepare_skill" => crate::agent_skill_authoring::prepare(root,session_id,run_id,args).await,
        "uninstall_skill" => crate::agent_skill_lifecycle::request_uninstall(root, session_id, run_id, str_field(args, "skill_id")?).await,
        "run_skill_script" => {
            let _files = crate::agent_skill_lifecycle::gate().lock().await;
            let id = str_field(args, "skill_id")?;
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) { return Err("任务已停止".into()); }
            let mut record = get(&mut db, NS, &format!("skill:{id}")).await?.ok_or("技能不存在")?;
            if record["enabled"] != true || record["deletedAt"].is_number() { return Err("技能已停用或卸载".into()); }
            if let Some(result) = crate::agent_skill_lifecycle::consent(&mut db, &s, &mut record, "run").await? { return Ok(result); }
            let script = str_field(args, "script")?.to_owned();
            let input = if args["input"].is_null() { json!({}) } else { args["input"].clone() };
            let timeout = args["timeout"].as_u64().unwrap_or(30);
            let root = root.to_path_buf();
            let run_hash = crate::agent_skill_runner::skill_dir(&record).ok().map(|dir|crate::agent_skill_runner::script_hash(&dir));
            let run_script = script.clone();
            let result = tauri::async_runtime::spawn_blocking(move || crate::agent_skill_runner::run_with_cancellation(&root, &record, &script, &input, timeout, &cancelled))
                .await.map_err(|e| e.to_string())?;
            if let Some(mut current) = get(&mut db,NS,&format!("skill:{id}")).await? {
                current["lastRun"] = json!({"at":now(),"script":run_script,"scriptHash":run_hash,"ok":result.as_ref().is_ok_and(|v|v["ok"] == true),
                    "error":match &result {Ok(v)=>v["toolError"]["message"].clone(),Err(e)=>json!(e)}});
                put(&mut db,NS,&format!("skill:{id}"),&current).await?;
            }
            result
        }
        "list_breakdowns" => Ok(json!({"projects":crate::agent_breakdown::list_projects(&mut db).await?,
            // 方法包导入的写法配方：不属于任何参考书，仿写时可以直接参考
            "templateRecipes":crate::agent_method_pack::template_recipes(&mut db).await?.iter()
                .map(|r| json!({"title":r["title"],"steps":r["steps"],"sourceTitle":r["projectTitle"]})).collect::<Vec<_>>()})),
        "read_breakdown_profile" => Ok(crate::agent_breakdown::profile_for_model(
            crate::agent_breakdown::profile(&mut db, &crate::agent_breakdown::project_id_arg(args)?).await?,
        )),
        "search_breakdown_cases" => {
            let only = args["project_id"].as_str().filter(|v| !v.is_empty());
            let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 30) as usize;
            Ok(json!({"cases":crate::agent_breakdown::search_cases(&mut db, str_field(args, "query")?, only, limit).await?}))
        }
        "read_pacing" => {
            // 作者自己这本书的节奏：与参考书画像同一口径，附平台画像的节奏参考值
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let points = crate::agent_breakdown::curve(&mut db, bid).await?;
            let profile_id = crate::agent_canon::book_profile_id(&mut db, bid).await?;
            let profile = crate::agent_canon::platform_profile(&mut db, &profile_id).await?;
            Ok(json!({"bookId":bid,"summary":crate::agent_breakdown::summarize(&points),"curve":points,
                "profile":{"id":profile_id,"name":profile["name"],"payoffEvery":profile["payoffEvery"],"midPayoffEvery":profile["midPayoffEvery"],
                    "suppressMaxChapters":profile["suppressMaxChapters"],"endHookRequired":profile["endHookRequired"]},
                "note":"数据来自章节卡；未索引章节不计入密度，并打断连续无回报区间，不能把未知当作没有回报"}))
        }
        "open_chapter_candidate" => {
            // 把一章正式正文复制成可修订的候选：模型不必把整章正文抄进参数；读取证据与写入时的版本核对照常
            let bid = str_field(args, "book_id")?;
            let cid = str_field(args, "chapter_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            let key = format!("artifact:{artifact_id}");
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            if let Some(mut saved) = get(&mut tx, NS, &key).await? {
                if saved["bookId"] != bid || saved["chapterId"] != cid {
                    return Err("重复操作编号对应不同章节".into());
                }
                let unbound = saved["rulesetId"].as_str().map_or(true, str::is_empty);
                let rules = crate::agent_ruleset::for_artifact(&mut tx, &s, &mut saved, args["ruleset_id"].as_str()).await?;
                if rules["toolError"].is_object() { return Ok(rules); }
                if unbound { crate::agent_submission::refresh_report(&mut tx, &mut saved).await?; }
                put(&mut tx, NS, &key, &saved).await?;
                tx.commit().await.map_err(err)?;
                let report = saved["reports"].as_array().and_then(|r| r.last()).cloned().unwrap_or(Value::Null);
                return Ok(json!({"artifact_id":artifact_id,"revision":saved["revision"],"expected_version":saved["expectedVersion"],"ruleset_id":saved["rulesetId"],
                    "summary":crate::agent_quality::summary(&report),"issues":issue_digest(&report)}));
            }
            let body = chapter(&mut tx, bid, cid).await?;
            let text = body["textContent"].as_str().unwrap_or("").to_owned();
            if text.trim().is_empty() {
                return Ok(json!({"ok":false,"toolError":{"code":"empty_chapter","retryable":false,
                    "message":"这一章还没有正文；写新内容用 draft_chapter"}}));
            }
            let mut evidence = body.clone();
            evidence["readRunId"] = json!(run_id);
            put(&mut tx, OP_NS, &format!("read:{session_id}:{bid}:{cid}"), &evidence).await?;
            let mut a = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"createdAt":now(),"bookId":bid,
                "bookTitle":book(&mut tx,bid).await?["title"],"chapterId":cid,"kind":"chapter",
                "title":body["title"].as_str().unwrap_or("章节").to_owned(),"original":text,"content":text,
                "expectedVersion":body["localVersion"],"applied":false,"revision":0,
                "sourceContext":get(&mut tx,OP_NS,&format!("context:{session_id}:{run_id}")).await?,
                "candidateSources":get(&mut tx,OP_NS,&format!("candidate-context:{session_id}:{run_id}")).await?});
            let rules = crate::agent_ruleset::for_artifact(&mut tx, &s, &mut a, args["ruleset_id"].as_str()).await?;
            if rules["toolError"].is_object() { return Ok(rules); }
            let report = crate::agent_quality::check_artifact(&mut tx, &a, &text, &artifact_id, 0, a["rulesetId"].as_str()).await?;
            a["textFingerprint"] = report["reviewedFingerprint"].clone();
            a["reports"] = json!([report]);
            put(&mut tx, NS, &key, &a).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"artifact_id":artifact_id,"revision":0,"expected_version":body["localVersion"],"ruleset_id":a["rulesetId"],
                "summary":crate::agent_quality::summary(&report),"issues":issue_digest(&report)}))
        }
        "compare_pacing" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            Ok(crate::agent_breakdown::compare_for_model(
                crate::agent_breakdown::compare(&mut db, bid, &crate::agent_breakdown::project_id_arg(args)?).await?,
            ))
        }
        "start_breakdown" => {
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = start_breakdown_tool(&mut tx, &s, session_id, args).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "breakdown_status" => {
            let pid = crate::agent_breakdown::project_id_arg(args)?;
            let p = crate::agent_breakdown::project(&mut db, &pid).await?;
            let cards = crate::agent_breakdown::active_card_chapters(&mut db, &crate::agent_breakdown::book_id(&pid)).await?.len();
            Ok(json!({"projectId":pid,"title":p["title"],"chapters":p["chapters"].as_array().map_or(0, |a| a.len()),"indexedChapters":cards,
                "job":crate::agent_breakdown_job::view(&mut db, &pid).await?,
                "note":"job.status：running 后台拆解中、paused 作者暂停、failed 需要作者处理后点继续、completed 已拆完、cancelled 已取消。进度卡在对话里，作者能看到和操作"}))
        }
        "read_material" => read_material(&mut db, &s, args).await,
        // 拆书报告：Python 侧分段归纳前读数据、写完后保存成果；只读本会话引用的参考书
        "breakdown_report_data" | "save_breakdown_report" => {
            let pid = crate::agent_breakdown::project_id_arg(args)?;
            allowed_source(&mut db, &s, &crate::agent_breakdown::book_id(&pid)).await?;
            if name == "breakdown_report_data" {
                return crate::agent_breakdown::report_data(&mut db, &pid, args["from"].as_i64().unwrap_or(1), args["to"].as_i64().unwrap_or(0)).await;
            }
            let op = format!("{name}:{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(done) = get(&mut db, OP_NS, &op).await? {
                return Ok(done);
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_breakdown::save_report(&mut tx, session_id, run_id, args).await?;
            if out["toolError"].is_null() {
                put(&mut tx, OP_NS, &op, &out).await?;
            }
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "save_name_candidates" => crate::agent_packaging::save_names(&mut db, &s, run_id, args).await,
        "save_book_intro" => crate::agent_packaging::save_intro(&mut db, &s, run_id, args).await,
        "request_cover" => crate::agent_packaging::save_cover_brief(&mut db, &s, run_id, args).await,
        "save_breakdown_recipe" | "save_comparison" => {
            let op = format!("{name}:{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(done) = get(&mut db, OP_NS, &op).await? {
                return Ok(done);
            }
            let out = if name == "save_breakdown_recipe" {
                crate::agent_breakdown::save_recipe(&mut db, &crate::agent_breakdown::project_id_arg(args)?,
                    str_field(args, "title")?, &args["steps"], "Agent 归纳").await?
            } else {
                let project_ids = args["project_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned).or_else(|| v.as_i64().map(|n| n.to_string())))
                    .collect::<Vec<_>>();
                if project_ids.is_empty() {
                    return Err("对照方案至少要关联一本对标书".into());
                }
                if let Some(error) = crate::agent_breakdown::check_comparison_rows(&mut db, &project_ids, &args["rows"]).await? {
                    return Ok(error);
                }
                let bid = args["book_id"].as_str().unwrap_or("");
                let book_title = if bid.is_empty() {
                    String::new()
                } else {
                    allowed_book(&mut db, &s, bid, false).await?;
                    book(&mut db, bid).await?["title"].as_str().unwrap_or("").to_owned()
                };
                let title = str_field(args, "title")?;
                let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
                let record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"title":title,"bookId":bid,"bookTitle":book_title,
                    "kind":"comparison","projectIds":project_ids,"rows":args["rows"],"content":crate::agent_breakdown::render_comparison(title, &args["rows"]),
                    "original":"","applied":false,"revision":0,"createdAt":now()});
                put(&mut db, NS, &format!("artifact:{artifact_id}"), &record).await?;
                record
            };
            if out["toolError"].is_null() {
                put(&mut db, OP_NS, &op, &out).await?;
            }
            Ok(out)
        }
        "plan_scenes" => {
            let bid = str_field(args, "book_id")?;
            let cid = str_field(args, "chapter_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            crate::agent_contract::validate_document("scenePlan", &args["plan"])?;
            let ruleset = crate::agent_ruleset::load(&mut db, str_field(&args["plan"], "rulesetId")?).await?;
            if ruleset["bookId"] != bid {
                return Err("规则集不属于当前作品".into());
            }
            let threads = crate::agent_canon::list_threads(&mut db, bid, None).await?;
            for touch in args["plan"]["threadsTouched"].as_array().into_iter().flatten() {
                if touch["action"] != "plant"
                    && !threads.iter().any(|t| t["threadId"] == touch["threadId"])
                {
                    return Ok(json!({"ok":false,"toolError":{"code":"invalid_thread","retryable":true,
                        "message":format!("推进或回收的线索不存在：{}；先 list_threads 取真实编号", touch["threadId"])}}));
                }
            }
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(saved) = get(&mut db, NS, &format!("artifact:{artifact_id}")).await? {
                return Ok(json!({"artifact_id":artifact_id,"revision":saved["revision"],"content":saved["content"]}));
            }
            let content = render_scenes(&args["plan"]);
            let record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"bookId":bid,
                "bookTitle":book(&mut db,bid).await?["title"],"chapterId":cid,"kind":"scenes","title":"场景安排",
                "plan":args["plan"],"content":content,"rulesetId":ruleset["rulesetId"],"original":"","applied":false,"revision":0,"createdAt":now()});
            put(&mut db, NS, &format!("artifact:{artifact_id}"), &record).await?;
            Ok(json!({"artifact_id":artifact_id,"revision":0,"content":content}))
        }
        "plan_chapters" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            crate::agent_contract::validate_document("chapterBatchPlan", &args["plan"])?;
            let ruleset = crate::agent_ruleset::load(&mut db, str_field(&args["plan"], "rulesetId")?).await?;
            if ruleset["bookId"] != bid {
                return Err("规则集不属于当前作品".into());
            }
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(saved) = get(&mut db, NS, &format!("artifact:{artifact_id}")).await? {
                return Ok(json!({"artifact_id":artifact_id,"revision":saved["revision"],"content":saved["content"]}));
            }
            let content = render_chapter_plan(&args["plan"]);
            let record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"bookId":bid,
                "bookTitle":book(&mut db,bid).await?["title"],"kind":"outline","title":format!("章纲批次：{}",args["plan"]["stageGoal"].as_str().unwrap_or("")),
                "plan":args["plan"],"content":content,"rulesetId":ruleset["rulesetId"],"original":"","applied":false,"revision":0,"createdAt":now()});
            put(&mut db, NS, &format!("artifact:{artifact_id}"), &record).await?;
            Ok(json!({"artifact_id":artifact_id,"revision":0,"content":content}))
        }
        "plan_short_story" => {
            // 结构卡：可以不挂作品；规则集必须是同一篇（同一作品或同一短篇画像）解析出来的
            let bid = args["book_id"].as_str().filter(|v| !v.is_empty());
            if let Some(bid) = bid {
                allowed_book(&mut db, &s, bid, false).await?;
            }
            crate::agent_contract::validate_document("shortStoryPlan", &args["plan"])?;
            let ruleset = crate::agent_ruleset::load(&mut db, str_field(&args["plan"], "rulesetId")?).await?;
            let owner = ruleset["bookId"].as_str().unwrap_or("");
            if bid.map_or(!owner.starts_with(crate::agent_canon::SHORT_PREFIX), |b| owner != b) {
                return Ok(json!({"ok":false,"toolError":{"code":"ruleset_mismatch","retryable":true,
                    "message":"规则集不属于这篇短篇：不挂作品时先 resolve_ruleset(profile_id=…)，挂作品时带同一个 book_id"}}));
            }
            let names = crate::agent_breakdown::session_reference_names(&mut db, &s).await?;
            if let Some(error) = crate::agent_breakdown::leak_error(&args["plan"].to_string(), &names) {
                return Ok(error);
            }
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(saved) = get(&mut db, NS, &format!("artifact:{artifact_id}")).await? {
                return Ok(json!({"artifact_id":artifact_id,"revision":saved["revision"],"content":saved["content"]}));
            }
            let content = render_short_plan(&args["plan"]);
            let logline: String = args["plan"]["topic"]["logline"].as_str().unwrap_or("").chars().take(30).collect();
            let book_title = match bid {
                Some(b) => book(&mut db, b).await?["title"].clone(),
                None => json!(""),
            };
            let record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"bookId":bid.unwrap_or(""),"bookTitle":book_title,
                "kind":"structure","title":format!("短篇结构卡：{logline}"),"plan":args["plan"],"content":content,
                "rulesetId":ruleset["rulesetId"],"profileId":ruleset["profileId"],"original":"","applied":false,"revision":0,"createdAt":now()});
            put(&mut db, NS, &format!("artifact:{artifact_id}"), &record).await?;
            Ok(json!({"artifact_id":artifact_id,"revision":0,"content":content}))
        }
        "short_begin" => {
            let pid = str_field(args, "plan_artifact_id")?;
            let plan = get(&mut db, NS, &format!("artifact:{pid}")).await?.ok_or("结构卡不存在")?;
            if plan["sessionId"] != session_id || plan["kind"] != "structure" {
                return Err("结构卡不属于本会话".into());
            }
            let bid = plan["bookId"].as_str().unwrap_or("").to_owned();
            if !bid.is_empty() {
                allowed_book(&mut db, &s, &bid, false).await?;
            }
            let owner = crate::agent_ruleset::load(&mut db, str_field(&plan, "rulesetId")?).await?["bookId"].as_str().unwrap_or("").to_owned();
            // 结构卡只存内容；规则在规划后变了就按原方法当场换成新版，结构卡上的编号只作来源记录
            let ruleset = crate::agent_ruleset::current(&mut db, &s, str_field(&plan, "rulesetId")?, &owner).await?;
            if ruleset["toolError"].is_object() {
                return Ok(ruleset);
            }
            let ruleset_id = str_field(&ruleset, "rulesetId")?.to_owned();
            let profile_id = ruleset["profileId"].as_str().unwrap_or(crate::agent_quality::SHORT_PROFILE).to_owned();
            let profile = crate::agent_canon::platform_profile(&mut db, &profile_id).await?;
            // 一次流式生成写不了三万字：默认取画像区间中点，封顶八千字
            let target_words = args["target_words"].as_u64().filter(|v| *v > 0).unwrap_or_else(|| {
                let words = &profile["chapterWords"];
                ((words[0].as_u64().unwrap_or(3000) + words[1].as_u64().unwrap_or(8000)) / 2).min(8000)
            });
            let variant: String = args["variant"].as_str().unwrap_or("").trim().chars().take(20).collect();
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            let key = format!("artifact:{artifact_id}");
            let writer_model = writing_model_identity(writing_model(&mut db,&s).await.as_ref());
            let record = match get(&mut db, NS, &key).await? {
                Some(existing) => existing,
                None => {
                    let logline: String = plan["plan"]["topic"]["logline"].as_str().unwrap_or("短篇").chars().take(30).collect();
                    let title = if variant.is_empty() { logline } else { format!("{logline}（{variant}）") };
                    let mut record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"bookId":bid,"bookTitle":plan["bookTitle"],
                        "kind":"short","batchJobId":s["batchJobId"],"batchIndex":s["batchStep"]["index"],"title":title,"original":"","content":"","partial":true,"rulesetId":ruleset_id,"profileId":profile_id,
                        "planArtifactId":pid,"variant":variant,"writerModel":writer_model,"targetWords":target_words,"applied":false,"revision":0,
                        "authorTask":crate::agent_task::instruction(&s),"createdAt":now()});
                    crate::agent_writing_basis::ensure(&mut db,&mut record).await?;
                    put(&mut db, NS, &key, &record).await?;
                    record
                }
            };
            let mut out = json!({"artifact_id":artifact_id,"revision":record["revision"],"writing_basis":record["writingBasis"],"plan_text":plan["content"],"rules_text":crate::agent_ruleset::writer_text(&ruleset),
                "target_words":target_words,"persona":profile["readerPersona"],"variant":variant,"complete":record["partial"] == false});
            if ruleset["refreshedFrom"].is_string() {
                out["ruleset_refreshed"] = json!({"from":ruleset["refreshedFrom"],"to":ruleset_id,"message":"规划后写作规则有变化，已按原方法改用最新规则起草"});
            }
            Ok(out)
        }
        "draft_begin" => {
            let bid = str_field(args, "book_id")?;
            let cid = str_field(args, "chapter_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            // 规则在规划后变了就按原方法当场换成新版；场景安排只存内容，上面的规则集编号只作来源记录
            let ruleset = crate::agent_ruleset::current(&mut db, &s, str_field(args, "ruleset_id")?, bid).await?;
            if ruleset["toolError"].is_object() {
                return Ok(ruleset);
            }
            let ruleset_id = str_field(&ruleset, "rulesetId")?;
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            let body = chapter(&mut db, bid, cid).await?;
            let plan = match args["plan_artifact_id"].as_str().filter(|v| !v.is_empty()) {
                Some(pid) => {
                    let p = get(&mut db, NS, &format!("artifact:{pid}"))
                        .await?
                        .ok_or("场景安排成果不存在")?;
                    if p["sessionId"] != session_id || p["chapterId"] != cid {
                        return Err("场景安排成果不属于本会话或本章".into());
                    }
                    if p["kind"] != "scenes" {
                        return Ok(json!({"ok":false,"toolError":{"code":"plan_format_outdated","retryable":true,
                            "message":"这份计划是旧的拍子格式；请用 plan_scenes 重新安排本章场景后再起草"}}));
                    }
                    Some(p)
                }
                None => None,
            };
            let scene_list = plan.as_ref().and_then(|p| p["plan"]["scenes"].as_array().cloned()).unwrap_or_default();
            let hints = scene_list
                .iter()
                .flat_map(|scene| scene["characters"].as_array().into_iter().flatten())
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let writer = writing_model(&mut db,&s).await;
            let (max_context, reserve) = match &writer {
                Some(m) => (
                    m["maxContext"].as_u64().unwrap_or(32000) as usize,
                    m["maxOutputTokens"].as_u64().unwrap_or(4096) as usize,
                ),
                None => (32000, 4096),
            };
            // 评测按写作方法与模型分组；同章不同方法各写一稿即为 A/B
            let writer_model = writing_model_identity(writer.as_ref());
            let method = ruleset["skillName"].as_str().unwrap_or("默认方法").to_owned();
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let generation_request = json!(crate::agent_task::latest_author_request(&s));
            let author_task = crate::agent_task::instruction(&s);
            // 模型可能在已交付后换调用编号、重新规划同一章。按作者请求复用成稿，
            // 不把这些新编号当成重新写一遍的授权；真正的新要求仍能创建新版。
            // 同编号的完整稿仍走原交付恢复；未完成稿也要检查，兼容升级前已启动的重复生成。
            let current = get(&mut tx, NS, &format!("artifact:{artifact_id}")).await?;
            if current.as_ref().is_none_or(|a| a["partial"] == true) {
                let existing = list(&mut tx, NS, &format!("artifact:{session_id}:")).await?
                    .into_iter()
                    .filter(|a| a["id"] != artifact_id && a["kind"] == "chapter" && a["bookId"] == bid && a["chapterId"] == cid
                        && a["partial"].is_boolean() && a["variant"] == method && a["writerModel"] == writer_model
                        && a["writingBasis"]["styleSkill"]["id"] == ruleset["styleSkill"]["id"]
                        && crate::agent_task::contains_run(&s, &a["runId"])
                        && (if a["generationRequestId"].is_string() {
                            a["generationRequestId"] == generation_request
                        } else {
                            a["authorTask"] == author_task
                                && json!(crate::agent_task::author_request_at(&s, a["createdAt"].as_i64().unwrap_or(0))) == generation_request
                        })
                        // 作者在编辑器改过正式正文时，不复用旧版本的结果。
                        && (if a["applied"] == true { &a["appliedVersion"] } else { &a["expectedVersion"] }) == &body["localVersion"])
                    .max_by_key(|a| (a["partial"] == false, a["createdAt"].as_i64().unwrap_or(0)));
                if let Some(existing) = existing {
                    if existing["partial"] == true {
                        return Ok(crate::agent_tool_boundary::failure("chapter_draft_in_progress",crate::agent_tool_boundary::FailureKind::Conflict,
                            "本次作者请求的这一章已有未完成稿件，不能另起一稿。请继续原写作任务，或向作者说明当前进度。",
                            false,"read_artifact",json!({"artifactId":existing["id"]})));
                    }
                    return Ok(json!({"artifact_id":existing["id"],"revision":existing["revision"],
                        "complete":true,"reused":true,"saved":existing["applied"] == true,
                        "state":if existing["applied"] == true { "saved" } else { "draft" },
                        "version":existing["appliedVersion"],"chapter_id":cid,"delivery":existing["delivery"],
                        "message":"本次作者请求的这一章已有完整稿件，已复用，没有重新生成。请汇报已有结果；需要修改时处理这份稿件，不要再次从头起草。"}));
                }
            }
            // 分层上下文仍用来记写作依据、取前文原文与人物状态；写作模型不再收到它的全文
            let mut context = crate::agent_context::assemble(
                &mut tx,
                Some(&s),
                &crate::agent_context::ContextRequest {
                    book_id: bid,
                    purpose: "write",
                    chapter_id: Some(cid),
                    ruleset_id: Some(ruleset_id),
                    query_hint: plan.as_ref().and_then(|p| p["content"].as_str()),
                    characters_hint: hints,
                    model_max_context: max_context,
                    output_reserve: reserve,
                    budget_tokens: None,
                },
            )
            .await?;
            if context["ok"] != true {
                tx.commit().await.map_err(err)?;
                return Ok(json!({"ok":false,"toolError":{"code":"budget_exceeded","retryable":false,
                    "message":"必要的规则或前文超出上下文预算，本次未生成；请按返回原因缩小批量范围、采用前文候选或整理规则，不能静默省略必要上下文","budgetExceeded":context["budgetExceeded"]}}));
            }
            let profile_id = crate::agent_canon::book_profile_id(&mut tx, bid).await?;
            let profile = crate::agent_canon::platform_profile(&mut tx, &profile_id).await?;
            let target_words = args["target_words"]
                .as_u64()
                .filter(|v| *v > 0)
                .unwrap_or_else(|| {
                    let words = &profile["chapterWords"];
                    (words[0].as_u64().unwrap_or(2000) + words[1].as_u64().unwrap_or(4000)) / 2
                });
            // 写作模型拿到的四样东西：作者要求与硬事实、范例原文、前文原文、场景要点
            let bound = crate::agent_style_profile::bound(&mut tx, &ruleset).await?;
            let author = crate::agent_style_profile::author_profile(&mut tx, bid).await?;
            let profiles: Vec<&Value> = bound.iter().chain(author.iter()).collect();
            let block_text = |layer: &str, types: &[&str]| -> Vec<String> {
                context["layers"][layer].as_array().into_iter().flatten()
                    .filter(|b| types.iter().any(|t| b["source"]["type"] == *t))
                    .filter_map(|b| b["text"].as_str().map(str::to_owned)).collect()
            };
            let mut facts = Vec::new();
            let materials = block_text("fixed", &["materials"]);
            if !materials.is_empty() { facts.push(format!("【核心设定与人物资料】\n{}", materials.join("\n"))); }
            let task_data = block_text("task", &["chapterPlan"]);
            if !task_data.is_empty() { facts.push(format!("【本章相关资料】\n{}", task_data.join("\n"))); }
            let related = block_text("related", &["memory", "threads", "chunk"]);
            if !related.is_empty() { facts.push(format!("【相关前文依据】\n{}", related.join("\n"))); }
            let preferences = block_text("fixed", &["author-memory"]);
            if !preferences.is_empty() { facts.push(format!("【作者偏好】\n{}", preferences.join("\n"))); }
            let rejected = block_text("fixed", &["memory"]);
            if !rejected.is_empty() { facts.push(format!("【绝不能当作已发生事实的否决内容】\n{}", rejected.join("\n"))); }
            let mut states = block_text("fixed", &["state"]);
            states.extend(block_text("related", &["state"]));
            if !states.is_empty() { facts.push(format!("【人物现状】\n{}", states.join("\n"))); }
            let adjacent = block_text("adjacent", &["cards", "volume"]);
            if !adjacent.is_empty() { facts.push(format!("【前文与当前阶段】\n{}", adjacent.join("\n"))); }
            let facts_text = facts.into_iter().filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n\n");
            let previous_tail = match context["layers"]["task"].as_array().into_iter().flatten().filter(|b| b["source"]["type"] == "candidate").last() {
                Some(candidate) => crate::agent_context::tail_by_paragraph(candidate["text"].as_str().unwrap_or(""), crate::agent_context::PREVIOUS_TAIL_CHARS),
                None => block_text("adjacent", &["previousTail"]).into_iter().next().unwrap_or_default(),
            };
            let mut scenes = Vec::new();
            let mut used: Vec<Value> = Vec::new();
            for scene in &scene_list {
                let kind = scene["kind"].as_str().unwrap_or("过渡");
                let points = scene["points"].as_str().unwrap_or("");
                let examples = crate::agent_style_profile::select_for_scene(&profiles, kind, points);
                for example in &examples {
                    if !used.iter().any(|e| e["id"] == example["id"]) { used.push(example.clone()); }
                }
                let names: Vec<&str> = scene["characters"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let scene_facts = states.iter().filter(|line| names.iter().any(|n| line.starts_with(n))).cloned().collect::<Vec<_>>().join("\n");
                scenes.push(json!({"no":scene["no"],"kind":kind,"points":points,"chapterGoal":plan.as_ref().map(|p|p["plan"]["chapterGoal"].clone()),"expectation":plan.as_ref().map(|p|p["plan"]["expectation"].clone()),"characters":scene["characters"],"examples":examples,"facts":scene_facts}));
            }
            let fallback_examples = if scene_list.is_empty() { crate::agent_style_profile::select_for_scene(&profiles, "过渡", plan.as_ref().and_then(|p| p["content"].as_str()).unwrap_or("")) } else { Vec::new() };
            for example in &fallback_examples { if !used.iter().any(|e| e["id"] == example["id"]) { used.push(example.clone()); } }
            // 写作依据里记的是本次真正给写作模型看的范例
            context["layers"]["style"] = json!(used.iter().map(|e| {
                let text = e["text"].as_str().unwrap_or("").to_owned();
                let mut source = e["source"].clone(); source["exampleId"] = e["id"].clone();
                json!({"label":"原文范例","tokens":crate::agent_context::estimate_tokens(&text),"text":text,"source":source})
            }).collect::<Vec<_>>());
            context["writerInput"] = json!({"factsText":facts_text,"previousTail":previous_tail,
                "scenes":scenes,"examples":fallback_examples});
            // 读取证据：写入时按这个版本校验，作者中途改稿会被识别
            let mut evidence = body.clone();
            evidence["readRunId"] = json!(run_id);
            put(&mut tx, OP_NS, &format!("read:{session_id}:{bid}:{cid}"), &evidence).await?;
            let key = format!("artifact:{artifact_id}");
            let record = match get(&mut tx, NS, &key).await? {
                Some(existing) => existing,
                None => {
                    let mut record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"bookId":bid,
                        "bookTitle":book(&mut tx,bid).await?["title"],"chapterId":cid,"kind":"chapter",
                        "title":body["title"].as_str().unwrap_or("章节").to_owned(),"original":body["textContent"],"content":"",
                        "partial":true,"expectedVersion":body["localVersion"],"rulesetId":ruleset_id,"profileId":profile_id,
                        "planArtifactId":args["plan_artifact_id"],"targetWords":target_words,"applied":false,"revision":0,
                        "variant":method,"writerModel":writer_model,"batchJobId":s["batchJobId"],"batchIndex":s["batchStep"]["index"],
                        "candidateSources":context["candidateSources"],"authorTask":crate::agent_task::instruction(&s),
                        "generationRequestId":generation_request,
                        "rulesetRefreshedFrom":ruleset["refreshedFrom"],"styleProfile":bound.as_ref().map(|p| p["source"].clone()),
                        "sourceContext":get(&mut tx,OP_NS,&format!("context:{session_id}:{run_id}")).await?,"createdAt":now()});
                    record["styleSampleIds"] = json!(used.iter().filter(|e| e["source"]["type"] == "sample").map(|e| e["id"].clone()).collect::<Vec<_>>());
                    record["writingBasis"] = crate::agent_writing_basis::capture(&mut tx,&record,&context,&ruleset).await?;
                    put(&mut tx, NS, &key, &record).await?;
                    record
                }
            };
            tx.commit().await.map_err(err)?;
            crate::agent_delivery::verify_sources(&mut db,&record).await?;
            let input = &record["writingBasis"]["writerInput"];
            if record["partial"] == true && !input.is_object() {
                return Ok(crate::agent_tool_boundary::failure("draft_input_outdated",crate::agent_tool_boundary::FailureKind::Precondition,
                    "旧候选缺少完整写作依据，已保留原稿；请重新安排场景并生成新的候选。",false,"plan_scenes",json!({"artifactId":artifact_id})));
            }
            let mut out = json!({"artifact_id":artifact_id,"revision":record["revision"],"expected_version":body["localVersion"],"complete":record["partial"] == false,
                "writing_basis":record["writingBasis"],"facts_text":input["factsText"],"previous_tail":input["previousTail"],"tail_chars":crate::agent_context::PREVIOUS_TAIL_CHARS,
                "scenes":input["scenes"],"examples":input["examples"],"completed_scenes":record["scenes"].as_array().cloned().unwrap_or_default(),"plan_text":plan.as_ref().and_then(|p|p["content"].as_str()).unwrap_or(""),
                "rules_text":crate::agent_ruleset::writer_text(&ruleset),"target_words":target_words,"estimated_tokens":context["estimatedTokens"],
                "persona":profile["readerPersona"]});
            if ruleset["refreshedFrom"].is_string() {
                out["ruleset_refreshed"] = json!({"from":ruleset["refreshedFrom"],"to":ruleset_id,"message":"规划后写作规则有变化，已按原方法改用最新规则起草"});
            }
            Ok(out)
        }
        "draft_scene" => {
            // 一个场景写完：把写到这里的正文记在稿子上，中断后续跑不重写已完成的场景，对话里也看得到进度
            let aid = str_field(args, "artifact_id")?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            let receipt_key = format!("draft-scene:{session_id}:{}", str_field(args, "operation_id")?);
            let request = json!({"artifactId":aid,"revision":args["expected_revision"],"scene":args["scene_no"],"text":args["text"]});
            if let Some(receipt) = get(&mut tx, OP_NS, &receipt_key).await? {
                if receipt["request"] == request { return Ok(receipt["result"].clone()); }
                return Ok(crate::agent_tool_boundary::failure("operation_intent_mismatch",crate::agent_tool_boundary::FailureKind::Conflict,
                    "同一场景提交对应不同内容，已有候选未覆盖。",false,"read_artifact",json!({"artifactId":aid})));
            }
            if a["sessionId"] != session_id || !crate::agent_quality::is_prose(&a) {
                return Err("成果不是本会话的章节候选".into());
            }
            if a["partial"] != true || args["expected_revision"].as_u64().unwrap_or(0) != a["revision"].as_u64().unwrap_or(0) {
                return Ok(crate::agent_tool_boundary::failure("draft_already_completed",crate::agent_tool_boundary::FailureKind::Conflict,
                    "生成阶段已结束或候选已被编辑，本场景结果未写入。",false,"read_artifact",json!({"artifactId":aid})));
            }
            let text = args["text"].as_str().unwrap_or("");
            if text.len() > 240_000 { return Err("章节内容超过单次修改上限".into()); }
            crate::agent_delivery::verify_sources(&mut tx,&a).await?;
            let no = args["scene_no"].clone();
            let entry = json!({"no":no,"text":text,"chars":words_of(text),"at":now()});
            let mut scenes = a["scenes"].as_array().cloned().unwrap_or_default();
            scenes.retain(|s| s["no"] != no);
            scenes.push(entry);
            a["scenes"] = json!(scenes);
            a["content"] = json!(text);
            put(&mut tx, NS, &key, &a).await?;
            let result = json!({"saved":true,"scene":no,"scenes":a["scenes"].as_array().map_or(0, |v| v.len())});
            put(&mut tx, OP_NS, &receipt_key, &json!({"request":request,"result":result})).await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "draft_finish" => {
            let aid = str_field(args, "artifact_id")?;
            let text = args["text"].as_str().ok_or("缺少正文")?;
            if text.len() > 240_000 {
                return Err("章节内容超过单次修改上限".into());
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            let receipt_key=format!("draft-finish:{session_id}:{}",str_field(args,"operation_id")?);
            let request=json!({"artifactId":aid,"fingerprint":crate::agent_quality::fingerprint(text)});
            if let Some(receipt)=get(&mut tx,OP_NS,&receipt_key).await? {
                if receipt["request"]==request {return Ok(receipt["result"].clone());}
                return Ok(crate::agent_tool_boundary::failure("operation_intent_mismatch",crate::agent_tool_boundary::FailureKind::Conflict,
                    "同一生成提交对应不同内容，未覆盖已有稿件。",false,"read_artifact",json!({})));
            }
            if a["sessionId"] != session_id || !crate::agent_quality::is_prose(&a) {
                return Err("成果不是本会话的章节候选".into());
            }
            if a["partial"]!=true || args["expected_revision"].as_u64().unwrap_or(0)!=a["revision"].as_u64().unwrap_or(0) {
                return Ok(crate::agent_tool_boundary::failure("draft_already_completed",crate::agent_tool_boundary::FailureKind::Conflict,
                    "生成阶段已结束或候选已被编辑，未覆盖当前版本。请使用修订流程处理已有稿件。",false,"read_artifact",json!({"artifactId":aid})));
            }
            let revision = a["revision"].as_i64().unwrap_or(0) + 1;
            let ruleset_id = a["rulesetId"].as_str().map(str::to_owned);
            let report = crate::agent_quality::check_artifact(&mut tx, &a, text, aid, revision, ruleset_id.as_deref()).await?;
            a["content"] = json!(text);
            // AI 原稿另存一份：作者之后的改动与它对比，归纳写作偏好
            if a["initialContent"].is_null() { a["initialContent"] = json!(text); }
            a["generatedContent"] = json!(text);
            a["generatedRevision"] = json!(revision);
            a["partial"] = json!(false);
            a["revision"] = json!(revision);
            a["textFingerprint"] = report["reviewedFingerprint"].clone();
            a["reports"] = json!([report]);
            put(&mut tx, NS, &key, &a).await?;
            let result=json!({"artifact_id":aid,"revision":revision,"blocked":report["blocked"],
                "summary":crate::agent_quality::summary(&report),"issues":issue_digest(&report)});
            put(&mut tx,OP_NS,&receipt_key,&json!({"request":request,"result":result})).await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "revise_begin" => {
            let aid = str_field(args, "artifact_id")?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            if a["sessionId"] != session_id {
                return Ok(json!({"ok":false,"toolError":{"code":"candidate_requires_copy","retryable":true,
                    "message":"该候选来自另一会话，请用 polish_draft 建立当前会话的修订副本。"}}));
            }
            if let Err(message)=crate::agent_delivery::accept_reviewed_revision(&mut a) {
                return Ok(crate::agent_tool_boundary::failure("revision_review_required",crate::agent_tool_boundary::FailureKind::Precondition,
                    &message,true,"check_chapter",json!({"artifactId":aid})));
            }
            if !crate::agent_quality::is_prose(&a) {
                return Err("成果不是本会话的章节候选或短篇稿".into());
            }
            let revision = a["revision"].as_i64().unwrap_or(0);
            if args["expected_revision"].as_i64().is_some_and(|e| e != revision) {
                return Ok(json!({"ok":false,"toolError":{"code":"stale_artifact","retryable":true,"message":"成果已被作者编辑，请 read_artifact 取最新 revision"}}));
            }
            let unbound = a["rulesetId"].as_str().map_or(true, str::is_empty);
            let rules = crate::agent_ruleset::for_artifact(&mut tx, &s, &mut a, None).await?;
            if rules["toolError"].is_object() { return Ok(rules); }
            crate::agent_delivery::verify_sources(&mut tx,&a).await?;
            if unbound { crate::agent_submission::refresh_report(&mut tx, &mut a).await?; }
            let content = a["content"].as_str().unwrap_or("").to_owned();
            let Some(report) = a["reports"].as_array().and_then(|r| r.last()).cloned() else {
                return Ok(json!({"ok":false,"toolError":{"code":"no_report","retryable":true,"message":"成果还没有体检报告，先 check_chapter"}}));
            };
            if report["reviewedFingerprint"] != crate::agent_quality::fingerprint(&content) {
                return Ok(json!({"ok":false,"toolError":{"code":"stale_report","retryable":true,"message":"报告对应的正文已变化，先重新 check_chapter"}}));
            }
            let wanted: Vec<&str> = args["issue_ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect();
            let paragraphs: Vec<&str> = content.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect();
            let instruction = args["instruction"].as_str().unwrap_or("").trim().to_owned();
            // 作者直接指定的段落：序号，或原文片段（在正文里唯一命中的那一段）
            let mut requested: std::collections::BTreeSet<usize> = args["paragraphs"].as_array().into_iter().flatten()
                .filter_map(|v| v.as_u64().map(|i| i as usize)).filter(|i| *i < paragraphs.len()).collect();
            for quote in args["quotes"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|q| !q.is_empty()) {
                let hits: Vec<usize> = paragraphs.iter().enumerate().filter(|(_, p)| p.contains(quote)).map(|(i, _)| i).collect();
                match hits.as_slice() {
                    [index] => { requested.insert(*index); }
                    [] => return Ok(crate::agent_tool_boundary::failure("quote_not_found",crate::agent_tool_boundary::FailureKind::Reference,
                        &format!("原文片段“{}”不在当前稿子里，请逐字复制当前版本的原文", quote.chars().take(40).collect::<String>()),true,"read_artifact",json!({"artifactId":aid}))),
                    _ => return Ok(crate::agent_tool_boundary::failure("quote_ambiguous",crate::agent_tool_boundary::FailureKind::Reference,
                        &format!("原文片段“{}”在稿子里出现不止一次，请给更长的片段或段落序号", quote.chars().take(40).collect::<String>()),true,"read_artifact",json!({"artifactId":aid}))),
                }
            }
            if !requested.is_empty() && instruction.is_empty() {
                return Ok(crate::agent_tool_boundary::failure("instruction_required",crate::agent_tool_boundary::FailureKind::Precondition,
                    "指定了要改的段落，但没有说明要怎么改；instruction 写作者的原话或具体要求",true,"",json!({})));
            }
            let mut targets: std::collections::BTreeMap<usize, Vec<Value>> = std::collections::BTreeMap::new();
            for index in &requested {
                targets.entry(*index).or_default().push(json!({"code":"author.request","message":instruction,"fix":instruction,"repairMode":"draft_edit"}));
            }
            for issue in report["issues"].as_array().into_iter().flatten() {
                let selected = if !wanted.is_empty() {
                    wanted.contains(&issue["id"].as_str().unwrap_or(""))
                } else if requested.is_empty() {
                    matches!(issue["level"].as_str(), Some("block") | Some("suspect"))
                } else {
                    false
                };
                if !selected {
                    continue;
                }
                let mut indices=issue["paragraphs"].as_array().into_iter().flatten().filter_map(|v|v.as_u64()).collect::<std::collections::BTreeSet<_>>();
                for range in issue["repairRanges"].as_array().into_iter().flatten() {
                    if let (Some(start),Some(end))=(range["start"].as_u64(),range["end"].as_u64()) {
                        if start<=end && end<paragraphs.len() as u64 { indices.extend(start..=end); }
                    }
                }
                for index in indices {
                    let index = index as usize;
                    if index < paragraphs.len() {
                        targets.entry(index).or_default().push(json!({"code":issue["code"],"message":issue["message"],"fix":issue["fix"],"repairMode":issue["repairMode"]}));
                    }
                }
            }
            let targets = targets
                .into_iter()
                .map(|(index, issues)| json!({"index":index,"text":paragraphs[index],"issues":issues}))
                .collect::<Vec<_>>();
            // 修订只拿作者的要求：方法技能、画像与文风技能正文不进写作模型
            let rules_text = crate::agent_ruleset::writer_text(&rules);
            let targeted_ids = report["issues"].as_array().into_iter().flatten().filter(|issue| {
                if !wanted.is_empty() { wanted.contains(&issue["id"].as_str().unwrap_or("")) } else if requested.is_empty() { matches!(issue["level"].as_str(), Some("block") | Some("suspect")) } else { false }
            }).map(|issue| issue["id"].clone()).collect::<Vec<_>>();
            put(&mut tx, OP_NS, &format!("revise-scope:{session_id}:{aid}:{revision}"),
                &json!({"runId":run_id,"fingerprint":crate::agent_quality::fingerprint(&content),"indices":targets.iter().map(|t|t["index"].clone()).collect::<Vec<_>>(),
                    "issueIds":targeted_ids,"instruction":instruction})).await?;
            put(&mut tx, NS, &key, &a).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"artifact_id":aid,"revision":revision,"content":content,"targets":targets,"rules_text":rules_text,
                "writing_basis":a["writingBasis"],
                "protected_context":crate::agent_delivery::protected_context(&mut db,&a).await?}))
        }
        "artifact_replace" => {
            let aid = str_field(args, "artifact_id")?;
            let op = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(done) = get(&mut db, OP_NS, &op).await? {
                return Ok(done["result"].clone());
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            if a["sessionId"] != session_id || !crate::agent_quality::is_prose(&a) {
                return Err("成果不是本会话的章节候选或短篇稿".into());
            }
            if a["applied"] == true {
                return Err("已写入作品的成果不能再修订，请在书籍中编辑".into());
            }
            let revision = a["revision"].as_i64().unwrap_or(0);
            if args["expected_revision"].as_i64().is_some_and(|e| e != revision) {
                return Ok(json!({"ok":false,"toolError":{"code":"stale_artifact","retryable":true,"message":"成果已被作者编辑，请 read_artifact 取最新 revision"}}));
            }
            let rules = crate::agent_ruleset::for_artifact(&mut tx, &s, &mut a, None).await?;
            if rules["toolError"].is_object() { return Ok(rules); }
            crate::agent_delivery::verify_sources(&mut tx,&a).await?;
            let content = a["content"].as_str().unwrap_or("").to_owned();
            let scope = get(&mut tx, OP_NS, &format!("revise-scope:{session_id}:{aid}:{revision}")).await?.ok_or("修订范围已失效，请重新读取待修订段落")?;
            if scope["runId"] != run_id || scope["fingerprint"] != crate::agent_quality::fingerprint(&content) { return Err("修订范围与当前正文不一致".into()); }
            let allowed: std::collections::BTreeSet<usize> = scope["indices"].as_array().into_iter().flatten().filter_map(|v|v.as_u64().map(|i|i as usize)).collect();
            let mut lines: Vec<Option<String>> = content.split('\n').map(|s|Some(s.to_owned())).collect();
            let index_map = paragraph_lines(&content);
            let mut occupied = std::collections::BTreeSet::new();
            let mut changed = 0;
            for edit in args["paragraphs"].as_array().ok_or("缺少段落")? {
                let index = edit["index"].as_u64().ok_or("段落序号无效")? as usize;
                let end = edit["endIndex"].as_u64().map(|v|v as usize).unwrap_or(index);
                if end < index || end >= index_map.len() || (index..=end).any(|i| !allowed.contains(&i) || occupied.contains(&i)) {
                    return Err("修订越过目标段落或范围重叠，本次没有改动".into());
                }
                occupied.extend(index..=end);
                let text = edit["text"].as_str().ok_or("缺少替换文字")?.trim();
                let first = index_map[index];
                let last = index_map[end];
                let previous = lines[first..=last].iter().filter_map(|line|line.as_deref()).collect::<Vec<_>>().join("\n");
                if previous.trim() == text { continue; }
                changed += end-index+1;
                for line in &mut lines[first..=last] { *line = None; }
                if !text.is_empty() { lines[first] = Some(text.to_owned()); }
                else {
                    // 删除段落时一并去掉其后空行，避免留下原地的大段空白。
                    let after = index_map.get(end+1).copied().unwrap_or(lines.len());
                    for line in &mut lines[last+1..after] { *line = None; }
                }
            }
            if changed == 0 { return Ok(json!({"artifact_id":aid,"revision":revision,"changed":0})); }
            let new_content = lines.into_iter().flatten().collect::<Vec<_>>().join("\n");
            if new_content.trim().is_empty() { return Err("修订不能删除整章，原候选已保留".into()); }
            let new_revision = revision + 1;
            let before = json!({"content":content,"revision":revision,"rulesetId":a["rulesetId"],"reports":a["reports"],"instruction":scope["instruction"]});
            let ruleset_id = a["rulesetId"].as_str().map(str::to_owned);
            let report = crate::agent_quality::check_artifact(&mut tx, &a, &new_content, aid, new_revision, ruleset_id.as_deref()).await?;
            let old_report = crate::agent_quality::check_artifact(&mut tx,&a,&content,aid,revision,ruleset_id.as_deref()).await?;
            let blocking = |r:&Value| r["issues"].as_array().into_iter().flatten().filter(|i|i["level"]=="block")
                .map(|i|json!([i["code"],i["quotes"]]).to_string()).collect::<std::collections::BTreeSet<_>>();
            if !blocking(&report).is_subset(&blocking(&old_report)) {
                let rejected = crate::agent_tool_boundary::failure("revision_regression",crate::agent_tool_boundary::FailureKind::Precondition,
                    "修订引入新的结构或数据错误，已保留修订前稿件与本次待查版本。",false,"read_artifact",json!({"artifactId":aid,"revision":revision}));
                put(&mut tx,OP_NS,&op,&json!({"request":args,"result":rejected,"before":before,"proposal":{"content":new_content,"report":report}})).await?;
                tx.commit().await.map_err(err)?;
                return Ok(rejected);
            }
            a["pendingRevision"] = json!({"operationId":str_field(args,"operation_id")?,"revision":new_revision,
                "fingerprint":report["reviewedFingerprint"],"before":before,"issueIds":scope["issueIds"]});
            for old in a["reports"].as_array_mut().into_iter().flatten() {
                old["stale"] = json!(true);
            }
            a["content"] = json!(new_content);
            // 修订后的段落也是 AI 写的：以修订稿为新的 AI 原稿，不把 AI 的改动算成作者偏好
            a["generatedContent"] = json!(new_content);
            a["generatedRevision"] = json!(new_revision);
            a["reviseRounds"] = json!(a["reviseRounds"].as_u64().unwrap_or(0) + 1);
            a["revision"] = json!(new_revision);
            a["textFingerprint"] = report["reviewedFingerprint"].clone();
            a["reports"].as_array_mut().ok_or("报告列表格式错误")?.push(report.clone());
            put(&mut tx, NS, &key, &a).await?;
            let result = json!({"artifact_id":aid,"revision":new_revision,"changed":changed,"blocked":report["blocked"],
                "summary":crate::agent_quality::summary(&report),"issues":issue_digest(&report),"report":report});
            put(&mut tx, OP_NS, &op, &json!({"request":args,"result":result,"before":before,"afterFingerprint":report["reviewedFingerprint"]})).await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "record_critic_failure" => {
            let aid = str_field(args, "artifact_id")?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            if a["sessionId"] != session_id || a["revision"] != args["expected_revision"] { return Err("成果已变化，未覆盖当前评审状态".into()); }
            if args["diagnostic"].is_object() {
                if !a["reviewFailures"].is_array(){a["reviewFailures"]=json!([]);}
                let failure=json!({"revision":a["revision"],"fingerprint":a["textFingerprint"],"basis":a["writingBasis"]["fingerprint"],
                    "diagnostic":args["diagnostic"],"createdAt":now()});
                a["reviewFailures"].as_array_mut().unwrap().push(failure);
            }
            if let Some(report) = a["reports"].as_array_mut().and_then(|r| r.last_mut()) {
                report["critic"] = json!({"status":"failed","error":str_field(args,"error")?.chars().take(160).collect::<String>()});
                if let Some(code)=args["error_code"].as_str() {report["critic"]["errorCode"]=json!(code);}
                if let Some(retryable)=args["retryable"].as_bool() {report["critic"]["retryable"]=json!(retryable);}
            }
            put(&mut tx, NS, &key, &a).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"recorded":true}))
        }
        "attach_critic" => {
            let aid = str_field(args, "artifact_id")?;
            crate::agent_contract::validate_document("criticReview", &args["review"])?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let key = format!("artifact:{aid}");
            let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
            if a["sessionId"] != session_id || !crate::agent_quality::is_prose(&a) {
                return Err("成果不是本会话的章节候选或短篇稿".into());
            }
            let revision = a["revision"].as_i64().unwrap_or(0);
            if args["expected_revision"].as_i64().is_some_and(|e| e != revision) {
                return Ok(json!({"ok":false,"toolError":{"code":"stale_artifact","retryable":true,"message":"成果已被作者编辑，请重新 check_chapter"}}));
            }
            crate::agent_delivery::verify_sources(&mut tx,&a).await?;
            let content = a["content"].as_str().unwrap_or("").to_owned();
            let mut review = args["review"].clone();
            crate::agent_delivery::reconcile_revision_review(&a, &mut review)?;
            let merged = {
                let reports = a["reports"].as_array_mut().ok_or("成果还没有体检报告，先 check_chapter")?;
                let Some(report) = reports.last_mut() else {
                    return Err("成果还没有体检报告，先 check_chapter".into());
                };
                if report["reviewedFingerprint"] != crate::agent_quality::fingerprint(&content) {
                    return Ok(json!({"ok":false,"toolError":{"code":"stale_report","retryable":true,"message":"报告对应的正文已变化，先重新 check_chapter"}}));
                }
                let issues = report["issues"].as_array_mut().ok_or("报告格式错误")?;
                issues.retain(|i| i["source"] != "critic");
                for (i, issue) in review["issues"].as_array().into_iter().flatten().enumerate() {
                    let mut item = issue.clone();
                    item["source"] = json!("critic");
                    item["id"] = json!(format!("critic:{i}:{}", issue["code"].as_str().unwrap_or("")));
                    // 引文必须在正文里；没有引文的只能是提示，评审不能产生阻断
                    let quotes: Vec<Value> = issue["quotes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|q| q.as_str().is_some_and(|s| !s.is_empty() && content.contains(s)))
                        .cloned()
                        .collect();
                    let level = match issue["level"].as_str() {
                        Some("block") | Some("suspect") if !quotes.is_empty() => "suspect",
                        _ => "notice",
                    };
                    item["level"] = json!(level);
                    item["quotes"] = json!(quotes);
                    let paragraphs = content.lines().map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>();
                    let mut located: Vec<Value> = issue["paragraphs"].as_array().into_iter().flatten().filter(|i|
                        i.as_u64().and_then(|index| paragraphs.get(index as usize)).is_some_and(|p|
                            item["quotes"].as_array().into_iter().flatten().any(|q|q.as_str().is_some_and(|q|p.contains(q)))))
                        .cloned().collect();
                    // The host owns paragraph numbering. Exact unique quotations
                    // can locate evidence even when the model miscounts an index.
                    // Repeated quotations still require an unambiguous supplied index.
                    for quote in item["quotes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        let found=paragraphs.iter().enumerate().filter(|(_,p)|p.contains(quote)).map(|(i,_)|i).collect::<Vec<_>>();
                        if found.len()==1 {
                            let index=json!(found[0]);
                            if !located.contains(&index) {located.push(index);}
                        }
                    }
                    located.sort_by_key(|v|v.as_u64().unwrap_or(0));
                    // Evidence anchors and editing scope have different jobs.
                    // A validated narrative block may include unquoted neighbours
                    // so the writer can remove repeats without leaving half an edit.
                    let ranges=issue["repairRanges"].as_array().into_iter().flatten().filter(|r|
                        match (r["start"].as_u64(),r["end"].as_u64()) {
                            (Some(start),Some(end)) => start<=end && end<paragraphs.len() as u64 &&
                                located.iter().any(|v|v.as_u64().is_some_and(|i|start<=i && i<=end)),
                            _ => false,
                        }).cloned().collect::<Vec<_>>();
                    item["repairRanges"]=json!(ranges);
                    item["evidenceStatus"]=json!(if located.is_empty() && item["repairMode"] != "none" {"unlocated"} else {"verified"});
                    if located.is_empty() { item["repairMode"] = json!("none"); }
                    item["paragraphs"] = json!(located);
                    issues.push(item);
                }
                report["critic"] = json!({"status":"available","summary":review["summary"]});
                if review["persona"].is_string() { report["critic"]["persona"] = review["persona"].clone(); }
                if review["revisionAssessment"].is_object() { report["critic"]["revisionAssessment"] = review["revisionAssessment"].clone(); }
                let mut check = report.clone();
                if let Some(map) = check.as_object_mut() {
                    map.remove("blocked");
                    map.remove("stale");
                }
                crate::agent_contract::validate_document("qualityReport", &check)?;
                report.clone()
            };
            put(&mut tx, NS, &key, &a).await?;
            tx.commit().await.map_err(err)?;
            let mut out = merged;
            out["summary"] = crate::agent_quality::summary(&out);
            Ok(out)
        }
        "list_artifacts" | "read_artifact" => {
            let bid = args["book_id"].as_str().filter(|v| !v.is_empty());
            if name == "read_artifact" {
                let aid = str_field(args, "artifact_id")?;
                return match crate::agent_artifacts::read(&mut db,&s,aid).await {
                    Ok(a) if bid.is_none_or(|b|a["bookId"] == b) => Ok(crate::agent_artifacts::model_view(&a)),
                    Ok(_) => Ok(json!({"ok":false,"toolError":{"code":"artifact_book_mismatch","retryable":true,
                        "message":"成果不属于指定作品，请依据 list_artifacts 返回的编号和作品重新读取。"}})),
                    Err(message) => Ok(json!({"ok":false,"toolError":{"code":"artifact_unavailable","retryable":true,
                        "message":message,"recovery":"用 list_artifacts 查询当前会话或已引用作品的现有成果，不要重复使用失效编号。"}})),
                };
            }
            if let Some(bid) = bid {
                allowed_book(&mut db, &s, bid, false).await?;
            }
            let items = list(&mut db, NS, "artifact:")
                .await?
                .into_iter()
                .filter(|a| !a["deletedAt"].is_number() && a["superseded"] != true && bid.map_or(a["sessionId"] == session_id, |bid| a["bookId"] == bid))
                .collect::<Vec<_>>();
            Ok(json!(items.iter().map(|a| json!({"id":a["id"],"title":a["title"],"kind":a["kind"],"bookId":a["bookId"],"chapterId":a["chapterId"],"sessionId":a["sessionId"],"revision":a["revision"],"applied":a["applied"]})).collect::<Vec<_>>()))
        }
        "apply_creation_artifact" => {
            let aid = str_field(args, "artifact_id")?;
            let a = get(&mut db, NS, &format!("artifact:{aid}"))
                .await?
                .ok_or("成果不存在")?;
            if a["revision"].as_u64().unwrap_or(0)
                != args["expected_revision"].as_u64().ok_or("缺少成果版本")?
            {
                return Err("成果已更新或不属于当前会话，请读取最新成果".into());
            }
            allowed_book(&mut db, &s, str_field(&a, "bookId")?, true).await?;
            // 候选比正文旧时，审批门已按同一事实向作者确认过覆盖
            let overwrite = apply_basis(&mut db, aid).await?["overwritesEdits"] == true;
            let applied =
                apply_artifact_with(root, aid, false, args["expected_revision"].as_u64(), overwrite)
                    .await?;
            if applied["kind"] == "material" {
                put(
                    &mut db,
                    OP_NS,
                    &format!(
                        "material-context:{session_id}:{}",
                        str_field(&applied, "bookId")?
                    ),
                    &json!({"runId":run_id,"revision":applied["materialRevision"]}),
                )
                .await?;
            }
            Ok(applied)
        }
        "create_volume" | "create_chapter" => {
            creation::create_catalog_entry(
                root,
                session_id,
                if name == "create_volume" {
                    "volume"
                } else {
                    "chapter"
                },
                args,
            )
            .await
        }
        "save_material" => {
            let direct = args["draft"] == false;
            if direct {
                allowed_book(&mut db, &s, str_field(args, "book_id")?, true).await?;
            }
            let candidate = creation::save_material_candidate(root, session_id, args).await?;
            if !direct || candidate["toolError"].is_object() {
                return Ok(candidate);
            }
            match creation::apply_material(root, str_field(&candidate, "id")?).await {
                Ok(saved) => {
                    put(
                        &mut db,
                        OP_NS,
                        &format!(
                            "material-context:{session_id}:{}",
                            str_field(&saved, "bookId")?
                        ),
                        &json!({"runId":run_id,"revision":saved["materialRevision"]}),
                    )
                    .await?;
                    Ok(saved)
                }
                Err(error) => {
                    let mut kept = candidate;
                    kept["saved"] = json!(false);
                    kept["error"] = json!(error);
                    Ok(kept)
                }
            }
        }
        "select_write_target" => {
            let bid = str_field(args, "book_id")?;
            allowed_book(&mut db, &s, bid, false).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let mut current = session(&mut tx, session_id).await?;
            current["targetBookId"] = json!(bid);
            current["contextBookId"] = json!(bid);
            save_session(&mut tx, &mut current).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"targetBookId":bid}))
        }
        "begin_new_task" => {
            // 模型判断作者这条消息开始了另一件事：作者要求、写入目标和技能选择从这一轮重新算，已有成果不动
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let mut current = session(&mut tx, session_id).await?;
            let opening = current["messages"].as_array().into_iter().flatten()
                .find(|m| m["role"] == "user" && m["runId"] == run_id && m["source"] != "breakdown")
                .cloned()
                .unwrap_or(Value::Null);
            current["taskId"] = json!(run_id);
            current["taskRunIds"] = json!([run_id]);
            current["taskInstruction"] = opening["text"].clone();
            current["targetBookId"] = Value::Null;
            current["selectedSkillId"] = opening["selectedSkill"]["id"].clone();
            save_session(&mut tx, &mut current).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"started":true,"message":"已按另一件事重新开始：之前的作者要求与写入目标不再沿用，需要写入时重新确定目标。"}))
        }
        "book_plan_adopt_begin" => crate::agent_book_adoption::begin(root, session_id, run_id, args).await,
        "book_plan_adopt_finish" => crate::agent_book_adoption::finish(root, session_id, run_id, args).await,
        "read_chapter" | "list_chapters" | "list_volumes" | "read_book_materials" => {
            let bid = str_field(args, "book_id")?;
            allowed_source(&mut db, &s, bid).await?;
            // 参考书（拆书库）不拆也能读：按章列目录、读原文；没有卷和作品资料，也不能写入
            if let Some(pid) = crate::agent_breakdown::project_of(bid) {
                return match name {
                    "list_chapters" => crate::agent_breakdown::catalog_for_model(&mut db, pid,
                        args["from_chapter"].as_i64().unwrap_or(1), args["to_chapter"].as_i64().unwrap_or(0)).await,
                    "read_chapter" => {
                        let mut c = crate::agent_breakdown::chapter(&mut db, pid, str_field(args, "chapter_id")?).await?;
                        c["reference"] = json!(true);
                        c["note"] = json!("参考书原文，只读；人物和情节不能进入作者作品");
                        Ok(c)
                    }
                    _ => Ok(json!({"ok":false,"toolError":{"code":"reference_book","retryable":false,
                        "message":"这是参考书，没有卷目录和作品资料；用 list_chapters、read_chapter、search_book 读原文，要分析写法用拆书工具"}})),
                };
            }
            if name == "read_chapter" {
                let cid = str_field(args, "chapter_id")?;
                let value = chapter(&mut db, bid, cid).await?;
                let mut evidence = value.clone();
                evidence["readRunId"] = json!(run_id);
                put(
                    &mut db,
                    OP_NS,
                    &format!("read:{session_id}:{bid}:{cid}"),
                    &evidence,
                )
                .await?;
                return Ok(value);
            }
            if name == "list_chapters" {
                return Ok(json!(catalog(&mut db, bid).await?));
            }
            if name == "list_volumes" {
                let rows:Vec<String>=sqlx::query_scalar("SELECT payload FROM local_volumes WHERE bookId=? AND deletedAt IS NULL ORDER BY sortNo,id")
                    .bind(bid).fetch_all(&mut db).await.map_err(err)?;
                return Ok(json!(rows
                    .iter()
                    .map(|r| serde_json::from_str::<Value>(r).map_err(err))
                    .collect::<Result<Vec<_>, _>>()?));
            }
            let doc = creation::materials(&mut db, bid).await?;
            let snapshot_key = format!("material-snapshot:{session_id}:{bid}:{}",creation::version(&doc));
            put(&mut db,OP_NS,&snapshot_key,&doc).await?;
            put(&mut db,OP_NS,&format!("material-read:{session_id}:{run_id}:{bid}"),&json!({"snapshotKey":snapshot_key})).await?;
            put(
                &mut db,
                OP_NS,
                &format!("read-materials:{session_id}:{bid}"),
                &doc,
            )
            .await?;
            put(
                &mut db,
                OP_NS,
                &format!("material-context:{session_id}:{bid}"),
                &json!({"runId":run_id,"revision":creation::story_version(&doc)}),
            )
            .await?;
            let mut response = doc.clone();
            response["contentVersion"] = json!(creation::version(&doc));
            Ok(response)
        }
        "recall_memory" => {
            let bid = args["book_id"].as_str().filter(|v| !v.is_empty());
            if let Some(bid) = bid {
                allowed_book(&mut db, &s, bid, false).await?;
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = memory::context(&mut tx, &s, bid).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "save_chapter" => {
            let bid = str_field(args, "book_id")?;
            let cid = str_field(args, "chapter_id")?;
            allowed_book(&mut db, &s, bid, true).await?;
            let op = format!("{session_id}:{}", str_field(args, "operation_id")?);
            let intent = save_chapter_intent(args);
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            if let Some(replay) = save_chapter_receipt(&mut tx, session_id, args).await? {
                return Ok(replay);
            }
            // 审批门之前核对过一遍；确认卡发出到作者点选之间正文可能又变了，写入前在同一事务里再核对一遍
            let ChapterSaveBasis { mut candidate, persist_candidate, key, overwrites_edits, current_text, .. } =
                match chapter_save_basis(&mut tx, session_id, run_id, args).await? {
                    Ok(basis) => basis,
                    Err(error) => return Ok(error),
                };
            let text = candidate["content"].as_str().ok_or("成果正文格式错误")?.to_owned();
            if overwrites_edits {
                // 作者已在确认卡上同意覆盖：候选从此以被覆盖的这一版为原文，撤销时回到的是它，不是候选生成时的旧文
                candidate["original"] = json!(current_text);
                candidate["expectedVersion"] = args["expected_version"].clone();
            }
            if candidate["sourceContext"].is_object() { memory::verify_proof(&mut tx, &candidate["sourceContext"], bid).await?; }
            if get(&mut tx, OP_NS, &op).await?.is_none() {
                if let Err(reason) = memory::verify_context(&mut tx, session_id, run_id, bid).await
                {
                    // 冲突时保留可见候选，生成内容不能只留在框架检查点里。
                    let aid = format!("{op}:context-conflict");
                    if let Some(kept) = get(&mut tx, NS, &format!("artifact:{aid}")).await? {
                        return Ok(kept);
                    }
                    let base = get(&mut tx, OP_NS, &format!("read:{session_id}:{bid}:{cid}"))
                        .await?
                        .ok_or("缺少章节原文")?;
                    let kept = json!({"id":aid,"sessionId":session_id,"bookId":bid,"chapterId":cid,
                        "bookTitle":book(&mut tx,bid).await?["title"],"title":"上下文变化后保留的稿子","kind":"chapter",
                        "original":base["textContent"],"content":text,"expectedVersion":args["expected_version"],
                        "applied":false,"saved":false,"revision":0,"error":reason,
                        "sourceContext":get(&mut tx,OP_NS,&format!("context:{session_id}:{run_id}")).await?});
                    put(&mut tx, NS, &format!("artifact:{aid}"), &kept).await?;
                    tx.commit().await.map_err(err)?;
                    return Ok(kept);
                }
            }
            if let Some(rejected) = crate::agent_submission::check(&mut tx, &mut candidate, Some(&s), args["skip_gate"] == true).await? {
                put(&mut tx, NS, &key, &candidate).await?;
                tx.commit().await.map_err(err)?;
                return Ok(rejected);
            }
            let result = store::save_chapter_in(
                &mut tx,
                bid,
                cid,
                &text,
                args["expected_version"].as_i64().ok_or("缺少读取版本")?,
                &op,
            )
            .await?;
            let mut saved = chapter(&mut tx, bid, cid).await?;
            saved["readRunId"] = json!(run_id);
            put(
                &mut tx,
                OP_NS,
                &format!("read:{session_id}:{bid}:{cid}"),
                &saved,
            )
            .await?;
            if persist_candidate {
                let a = &mut candidate;
                a["appliedSourceRevision"] = a["revision"].clone();
                a["applied"] = json!(true);
                a["appliedVersion"] = result["version"].clone();
                a["revision"] = json!(a["revision"].as_u64().unwrap_or(0) + 1);
                let key = format!("artifact:{}", str_field(a, "id")?);
                put(&mut tx, NS, &key, a).await?;
            }
            let mut receipt = get(&mut tx, OP_NS, &op).await?.ok_or("保存回执缺失")?;
            receipt["toolIntent"] = intent;
            put(&mut tx, OP_NS, &op, &receipt).await?;
            tx.commit().await.map_err(err)?;
            Ok(result)
        }
        "save_selection_cards" => {
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(saved) = get(&mut db, NS, &format!("artifact:{artifact_id}")).await? {
                return Ok(saved);
            }
            let mut cards = args["cards"].as_array().cloned().unwrap_or_default();
            if cards.is_empty() {
                return Err("至少需要一张选题卡".into());
            }
            for card in &cards {
                crate::agent_contract::validate_document("selectionCard", card)?;
            }
            // 选题卡不属于任何作品；数据事实逐条对照本地快照，结果随卡保存，界面据此标出待核实。
            for card in cards.iter_mut() {
                if let Some(evidence) = card["evidence"].as_array_mut() {
                    for item in evidence.iter_mut() {
                        let check = crate::agent_rank::check_evidence(&mut db, item).await?;
                        item["check"] = check;
                    }
                }
            }
            let content = render_selection_cards(&cards);
            let v = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"title":str_field(args,"title")?,
                "bookId":"","bookTitle":"","kind":"selection","selectionCards":cards,"content":content,
                "original":"","applied":false,"revision":0,"createdAt":now()});
            put(&mut db, NS, &format!("artifact:{artifact_id}"), &v).await?;
            Ok(v)
        }
        // 自动任务：模型先看已有任务再建，建立与删除都经审批门的确认卡（频率与估算用量写在卡上）
        "list_automations" => Ok(json!({"tasks":crate::agent_scheduler::tasks_view(&mut db).await?})),
        "create_automation" => {
            if let Some(bid) = args["book_id"].as_str().filter(|v| !v.is_empty()) {
                allowed_book(&mut db, &s, bid, false).await?;
            }
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = crate::agent_scheduler::create(&mut tx, &s, args).await?;
            tx.commit().await.map_err(err)?;
            Ok(out)
        }
        "delete_automation" => {
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let deleted = crate::agent_scheduler::delete_task(&mut tx, str_field(args, "id")?).await?;
            tx.commit().await.map_err(err)?;
            Ok(json!({"deleted":deleted}))
        }
        "save_artifact" => {
            let bid = args["book_id"].as_str().unwrap_or("");
            let mut original = String::new();
            if !bid.is_empty() {
                allowed_book(&mut db, &s, bid, false).await?;
            }
            if let Some(cid) = args["chapter_id"].as_str() {
                let base = get(&mut db, OP_NS, &format!("read:{session_id}:{bid}:{cid}"))
                    .await?
                    .ok_or("生成章节候选前必须读取正文")?;
                if base["localVersion"] != args["expected_version"] {
                    return Err("候选稿缺少对应版本的原文".into());
                }
                original = base["textContent"].as_str().unwrap_or("").into();
            }
            let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
            if let Some(saved) = get(&mut db, NS, &format!("artifact:{artifact_id}")).await? {
                return Ok(saved);
            }
            let source_context =
                get(&mut db, OP_NS, &format!("context:{session_id}:{run_id}")).await?;
            let v = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"createdAt":now(),"batchJobId":s["batchJobId"],"batchIndex":s["batchStep"]["index"],"title":str_field(args,"title")?,
                "bookId":bid,"bookTitle":if bid.is_empty(){String::new()}else{book(&mut db,bid).await?["title"].as_str().unwrap_or("").into()},
                "chapterId":args["chapter_id"],"expectedVersion":args["expected_version"],"kind":args["kind"],
                "original":original,"content":str_field(args,"content")?,"applied":false,"revision":0,"sourceContext":source_context,
                "candidateSources":get(&mut db,OP_NS,&format!("candidate-context:{session_id}:{run_id}")).await?});
            put(&mut db, NS, &format!("artifact:{artifact_id}"), &v).await?;
            Ok(v)
        }
        _ => Err("工具尚未接入，不能声称已执行".into()),
    }
}

/// save_chapter 的保存依据：候选是哪一份、正文现在是哪一版、本会话读的是不是现在这一版。
/// 发确认卡之前和真正写入之前各核对一遍；核对不过走 `Err` 分支返回工具错误，作者不会为一次注定失败的保存点确认。
pub struct ChapterSaveBasis {
    pub candidate: Value,
    pub persist_candidate: bool,
    pub key: String,
    /// 正文现在的版本；已核对等于模型报的 expected_version
    pub current_version: i64,
    /// 正文现在的全文：候选覆盖它时记进候选的 original，撤销才能回到这一版
    pub current_text: String,
    /// 候选基于的正文版本比现在的旧：写入会覆盖候选生成后作者（或别的会话）对这一章的改动
    pub overwrites_edits: bool,
    pub current_words: usize,
}

impl ChapterSaveBasis {
    /// 交给审批门的事实：确认卡据此决定问不问、怎么问
    pub fn facts(&self) -> Value {
        json!({"overwritesEdits":self.overwrites_edits,"currentWords":self.current_words,"currentVersion":self.current_version})
    }
}

fn save_chapter_intent(args: &Value) -> Value {
    json!([args["book_id"], args["chapter_id"], args["expected_version"], args["artifact_id"], args["expected_artifact_revision"], args["text"]])
}

/// 同一操作编号的重放直接取回执；编号相同内容不同是错误
async fn save_chapter_receipt(db: &mut SqliteConnection, session_id: &str, args: &Value) -> Result<Option<Value>, String> {
    let op = format!("{session_id}:{}", str_field(args, "operation_id")?);
    let Some(receipt) = get(db, OP_NS, &op).await? else { return Ok(None) };
    if receipt["toolIntent"] == save_chapter_intent(args) {
        return Ok(Some(receipt["result"].clone()));
    }
    if receipt["toolIntent"].is_null() {
        return Ok(None);
    }
    Err("重复操作编号对应不同内容".into())
}

fn words_of(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

pub async fn chapter_save_basis(
    db: &mut SqliteConnection,
    session_id: &str,
    run_id: &str,
    args: &Value,
) -> Result<Result<ChapterSaveBasis, Value>, String> {
    let bid = str_field(args, "book_id")?;
    let cid = str_field(args, "chapter_id")?;
    let op = format!("{session_id}:{}", str_field(args, "operation_id")?);
    let expected = args["expected_version"].as_i64().ok_or("缺少读取版本")?;
    let artifact_id = args["artifact_id"].as_str().filter(|v| !v.is_empty());
    let base = get(db, OP_NS, &format!("read:{session_id}:{bid}:{cid}")).await?.ok_or("修改前必须读取章节")?;
    let candidate = match artifact_id {
        Some(aid) => {
            let a = get(db, NS, &format!("artifact:{aid}")).await?.ok_or("成果不存在")?;
            if a["sessionId"] != session_id || a["kind"] != "chapter" || a["bookId"] != bid || a["chapterId"] != cid {
                return Err("成果不是本会话对应章节的候选".into());
            }
            if args["expected_artifact_revision"].as_u64() != a["revision"].as_u64() {
                return Ok(Err(json!({"ok":false,"toolError":{"code":"stale_artifact","retryable":true,
                    "message":"请 read_artifact 取得当前 revision；保存必须指明这一版本"}})));
            }
            a
        }
        None => json!({"id":format!("{op}:submission"),"sessionId":session_id,"runId":run_id,"bookId":bid,"chapterId":cid,
            "kind":"chapter","title":"章节修改稿","original":base["textContent"],"content":args["text"].as_str().ok_or("缺少正文或成果编号")?,
            "expectedVersion":expected,"revision":0,"applied":false,"createdAt":now(),
            "candidateSources":get(db,OP_NS,&format!("candidate-context:{session_id}:{run_id}")).await?}),
    };
    // 模型报的版本必须是本会话最近一次读到的版本：它说明模型知道正文现在是什么样
    if base["localVersion"].as_i64() != Some(expected) {
        return Ok(Err(json!({"ok":false,"toolError":{"code":"stale_read","retryable":true,"recoveryTool":"read_chapter",
            "message":format!("expected_version 要用最近一次 read_chapter 读到的 localVersion（本会话读到的是 {}），不是候选生成时的版本；候选比正文旧时平台会向作者确认是否覆盖", base["localVersion"])}})));
    }
    // 读取之后正文又变了（作者在编辑器里改过、别的会话写过）：先重读，确认卡不能描述一个已经过期的对象
    let current = chapter(db, bid, cid).await?;
    let current_version = current["localVersion"].as_i64().unwrap_or(0);
    if current_version != expected {
        return Ok(Err(json!({"ok":false,"toolError":{"code":"stale_read","retryable":true,"recoveryTool":"read_chapter",
            "message":format!("正文在读取后改过（现在是版本 {current_version}）：先 read_chapter 再保存")}})));
    }
    let text = candidate["content"].as_str().ok_or("成果正文格式错误")?;
    if text.len() > 240_000 {
        return Err("章节内容超过单次修改上限".into());
    }
    let key = format!("artifact:{}", str_field(&candidate, "id")?);
    let persist_candidate = artifact_id.is_some() || get(db, NS, &key).await?.is_some();
    let current_text = current["textContent"].as_str().unwrap_or("").to_owned();
    Ok(Ok(ChapterSaveBasis {
        overwrites_edits: candidate["expectedVersion"].as_i64() != Some(current_version),
        current_words: words_of(&current_text),
        candidate,
        persist_candidate,
        key,
        current_version,
        current_text,
    }))
}

#[cfg(test)]
pub async fn apply_artifact(root: &Path, artifact_id: &str, undo: bool) -> Result<Value, String> {
    apply_artifact_checked(root, artifact_id, undo, None).await
}

#[cfg(test)]
pub async fn apply_artifact_checked(
    root: &Path,
    artifact_id: &str,
    undo: bool,
    expected: Option<u64>,
) -> Result<Value, String> {
    apply_artifact_with(root, artifact_id, undo, expected, false).await
}

/// 采用章节候选前的事实：候选基于的正文版本比现在的旧（作者在候选生成后改过这一章）时，采用会覆盖这些改动。
/// 成果面板与模型工具都先问这里，再决定要不要向作者确认覆盖。
pub async fn apply_basis(db: &mut SqliteConnection, artifact_id: &str) -> Result<Value, String> {
    let a = get(db, NS, &format!("artifact:{artifact_id}")).await?.ok_or("成果不存在")?;
    if a["kind"] != "chapter" || a["applied"] == true {
        return Ok(json!({"overwritesEdits":false}));
    }
    let (Some(bid), Some(cid)) = (a["bookId"].as_str(), a["chapterId"].as_str().map(str::to_owned).or_else(|| a["chapterId"].as_i64().map(|v| v.to_string()))) else {
        return Ok(json!({"overwritesEdits":false}));
    };
    let current = chapter(db, bid, &cid).await?;
    let current_version = current["localVersion"].as_i64().unwrap_or(0);
    let text = current["textContent"].as_str().unwrap_or("");
    Ok(json!({"overwritesEdits":a["expectedVersion"].as_i64() != Some(current_version),"currentVersion":current_version,
        "candidateVersion":a["expectedVersion"],"currentWords":text.chars().filter(|c| !c.is_whitespace()).count()}))
}

/// `overwrite`：作者已明确同意覆盖候选生成后对这一章的改动；此时按正文现在的版本写入，候选的原文换成被覆盖的那一版。
pub async fn apply_artifact_with(
    root: &Path,
    artifact_id: &str,
    undo: bool,
    expected: Option<u64>,
    overwrite: bool,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let key = format!("artifact:{artifact_id}");
    let mut a = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
    if a["kind"] == "material" {
        if undo {
            return Err("资料已正式保存，请通过资料编辑提交新的修改".into());
        }
        drop(tx);
        return creation::apply_material_checked(root, artifact_id, expected).await;
    }
    let sid = str_field(&a, "sessionId")?;
    let s = session(&mut tx, sid).await?;
    let book_id = str_field(&a, "bookId")?.to_owned();
    let bid = book_id.as_str();
    // 作者点击应用/撤销，已明确指定成果中的书籍；仍校验其来源范围。
    allowed_book(&mut tx, &s, bid, false).await?;
    if a["kind"] == "intro" {
        if undo != (a["applied"] == true) {
            return Ok(a);
        }
        if expected.is_some_and(|revision| revision != a["revision"].as_u64().unwrap_or(0)) {
            return Err("简介已被作者编辑，请读取最新成果后继续".into());
        }
        crate::agent_packaging::apply_intro(&mut tx, &mut a, undo).await?;
        put(&mut tx, NS, &key, &a).await?;
        tx.commit().await.map_err(err)?;
        return Ok(a);
    }
    if a["kind"] != "chapter" {
        return Err("此成果仅作为创作资料保存，请在对话中明确后续操作".into());
    }
    let applied = a["applied"] == true;
    if undo != applied {
        return Ok(a);
    }
    if expected.is_some_and(|revision| revision != a["revision"].as_u64().unwrap_or(0)) {
        return Err("成果已被作者编辑，请读取最新成果后继续".into());
    }
    if !undo {
        if let Some(rejected) = crate::agent_submission::check(&mut tx, &mut a, None, false).await? {
            put(&mut tx, NS, &key, &a).await?;
            tx.commit().await.map_err(err)?;
            return Err(rejected["toolError"]["message"].as_str().unwrap_or("当前稿件不能保存").to_owned());
        }
    }
    if !undo && a["sourceContext"].is_object() {
        memory::verify_proof(&mut tx, &a["sourceContext"], bid).await?;
    }
    if !undo {
        let current = chapter(&mut tx, bid, str_field(&a, "chapterId")?).await?;
        let current_version = current["localVersion"].as_i64().unwrap_or(0);
        if a["expectedVersion"].as_i64() != Some(current_version) {
            if !overwrite {
                let words = current["textContent"].as_str().unwrap_or("").chars().filter(|c| !c.is_whitespace()).count();
                return Err(format!("这一章在稿子生成后改过（现在约 {words} 字），保存会覆盖这些改动；请确认覆盖后再保存"));
            }
            // 作者同意覆盖：候选从此以被覆盖的这一版为原文，撤销回到的是它
            a["original"] = current["textContent"].clone();
            a["expectedVersion"] = json!(current_version);
        }
    }
    let version = if undo {
        a["appliedVersion"].as_i64()
    } else {
        a["expectedVersion"].as_i64()
    }
    .ok_or("缺少原文版本，需重新生成修改")?;
    let result = store::save_chapter_in(
        &mut tx,
        bid,
        str_field(&a, "chapterId")?,
        a[if undo { "original" } else { "content" }]
            .as_str()
            .ok_or("成果正文格式错误")?,
        version,
        &format!("artifact:{artifact_id}:{undo}:{version}"),
    )
    .await?;
    if !undo { a["appliedSourceRevision"] = a["revision"].clone(); }
    a["applied"] = json!(!undo);
    a["appliedVersion"] = result["version"].clone();
    if undo {
        a["expectedVersion"] = result["version"].clone();
        if let Some(checks) = a["sourceContext"]["chapters"].as_array_mut() {
            for check in checks {
                if check["source"]["chapterId"] == result["chapterId"] {
                    check["revision"] =
                        json!(memory::source_value(&mut tx, &check["source"]).await?.0);
                }
            }
        }
    }
    a["revision"] = json!(a["revision"].as_u64().unwrap_or(0) + 1);
    put(&mut tx, NS, &key, &a).await?;
    tx.commit().await.map_err(err)?;
    Ok(a)
}

// ---------------------------------------------------------------------------
// 写作工具的辅助：作者原话核验、成果渲染、段落定位
// ---------------------------------------------------------------------------

/// 作者原话摘录是否出自本任务里作者自己的消息（含答卡）；作者授权类操作（改线索状态、存规则、裁决疑点）都以此为依据。
/// 换了一轮不作废；宿主代发的续跑提示不算作者的话。
pub fn author_said_any(session: &Value, needles: &[&str]) -> bool {
    crate::agent_task::author_messages(session)
        .into_iter()
        .any(|m| {
            m["text"]
                .as_str()
                .is_some_and(|t| needles.iter().any(|n| !n.is_empty() && t.contains(n)))
        })
}

/// 拍子成果的可读文本；模型写正文时按它推进。
/// 结构卡给作者看、也给写作模型用的文本。
pub fn render_short_plan(plan: &Value) -> String {
    let topic = &plan["topic"];
    let mut lines = vec![
        format!("选题：{}｜情绪核：{}｜身份反差：{}", topic["genre"].as_str().unwrap_or(""), topic["emotionCore"].as_str().unwrap_or(""), topic["contrast"].as_str().unwrap_or("")),
        format!("一句话梗：{}", topic["logline"].as_str().unwrap_or("")),
        String::new(),
        "开篇爆点句候选：".to_owned(),
    ];
    for (i, line) in plan["openingLines"].as_array().into_iter().flatten().enumerate() {
        lines.push(format!("{}. {}", i + 1, line.as_str().unwrap_or("")));
    }
    lines.push(String::new());
    lines.push(format!("主线：我要{}，障碍是{}", plan["mainline"]["want"].as_str().unwrap_or(""), plan["mainline"]["obstacle"].as_str().unwrap_or("")));
    lines.push(String::new());
    lines.push("反转清单：".to_owned());
    for (i, twist) in plan["twists"].as_array().into_iter().flatten().enumerate() {
        lines.push(format!("{}. 读者以为：{} → 实际是：{}", i + 1, twist["readerThinks"].as_str().unwrap_or(""), twist["actually"].as_str().unwrap_or("")));
    }
    lines.push(String::new());
    lines.push(format!("结尾：{}｜回味：{}", plan["ending"]["type"].as_str().unwrap_or(""), plan["ending"]["afterTaste"].as_str().unwrap_or("")));
    let order = plan["infoOrder"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>();
    if !order.is_empty() {
        lines.push(String::new());
        lines.push(format!("信息释放顺序：{}", order.join(" → ")));
    }
    lines.join("\n")
}

/// 场景安排的可读文本：给作者看，也给编辑评审核对“要点有没有写到”。
pub fn render_scenes(plan: &Value) -> String {
    let mut lines = vec![format!("本章目标：{}", plan["chapterGoal"].as_str().unwrap_or(""))];
    for scene in plan["scenes"].as_array().into_iter().flatten() {
        let names = scene["characters"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("、");
        let who = if names.is_empty() { String::new() } else { format!("（人物：{names}）") };
        lines.push(format!("场景 {}［{}］{}{}", scene["no"], scene["kind"].as_str().unwrap_or(""), scene["points"].as_str().unwrap_or(""), who));
    }
    let info = plan["infoRelease"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>();
    if !info.is_empty() {
        lines.push(format!("本章释放的信息：{}", info.join("；")));
    }
    for touch in plan["threadsTouched"].as_array().into_iter().flatten() {
        lines.push(format!("线索：{} {}（{}）", touch["action"].as_str().unwrap_or(""), touch["text"].as_str().unwrap_or(""), touch["threadId"].as_str().unwrap_or("新")));
    }
    if let Some(expectation) = plan["expectation"].as_str().filter(|e| !e.is_empty()) {
        lines.push(format!("留给读者的期待：{expectation}"));
    }
    lines.join("\n")
}

/// 章纲批次的可读文本：章纲清单 + 期待-兑现表。
/// 选题卡的文本形式：供 read_artifact 与字数统计使用，证据后标出核对结果。
pub fn render_selection_cards(cards: &[Value]) -> String {
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    let join = |value: &Value| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("、")
    };
    let mut blocks = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let mut lines = vec![
            format!("选题{}：{}", index + 1, text(&card["genre"])),
            format!("卖点：{}", text(&card["sellingPoint"])),
            format!("主角与目标：{}", text(&card["protagonist"])),
            format!("开局：{}", text(&card["opening"])),
            format!("对标书：{}", join(&card["benchmarks"])),
            format!("风险：{}", join(&card["risks"])),
            "证据：".to_owned(),
        ];
        for item in card["evidence"].as_array().into_iter().flatten() {
            // 数据证据只展示宿主从快照读出的文字；模型的话只出现在判断里
            let data = || {
                let shown = text(&item["check"]["text"]);
                if shown.is_empty() {
                    format!("榜单 {}（{} 快照）", item["sourceId"], text(&item["snapshotDate"]))
                } else {
                    shown
                }
            };
            lines.push(match item["check"]["status"].as_str() {
                Some("verified") => format!("  - {}［已核对］", data()),
                Some("unverified") => format!("  - {}［待核实：{}］", data(), text(&item["check"]["issue"])),
                _ => format!("  - {}（{}）［判断］", text(&item["fact"]), text(&item["source"])),
            });
        }
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

pub fn render_chapter_plan(plan: &Value) -> String {
    let mut lines = vec![format!("阶段目标：{}", plan["stageGoal"].as_str().unwrap_or(""))];
    for entry in plan["entries"].as_array().into_iter().flatten() {
        lines.push(format!(
            "第{}章 {}\n  章纲：{}\n  钩子：{}{}",
            entry["no"],
            entry["title"].as_str().unwrap_or(""),
            entry["outline"].as_str().unwrap_or(""),
            entry["hookType"].as_str().unwrap_or(""),
            entry["threads"]
                .as_array()
                .filter(|t| !t.is_empty())
                .map(|t| format!("｜线索：{}", t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、")))
                .unwrap_or_default()
        ));
    }
    lines.push("期待-兑现表：".to_owned());
    for entry in plan["entries"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  第{}章｜给出：{}｜兑现：{}{}",
            entry["no"],
            entry["expectation"].as_str().unwrap_or(""),
            entry["payoff"].as_str().unwrap_or("—"),
            entry["payoffType"].as_str().map(|t| format!("（{t}）")).unwrap_or_default()
        ));
    }
    lines.join("\n")
}

/// 正文按非空段落编号（与质量门一致），返回每个段落在原始行数组里的位置。
pub fn paragraph_lines(content: &str) -> Vec<usize> {
    content
        .split('\n')
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .collect()
}

/// 报告问题的精简清单，给模型循环用；完整报告在成果里。
pub fn issue_digest(report: &Value) -> Vec<Value> {
    report["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| json!({"id":i["id"],"code":i["code"],"level":i["level"],"message":i["message"],"paragraphs":i["paragraphs"],"fix":i["fix"]}))
        .collect()
}
