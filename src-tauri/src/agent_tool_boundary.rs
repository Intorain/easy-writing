//! Semantic tool boundary. Generated policies declare resource kinds and effects;
//! storage keys never define access, and operational errors are not parsed as prose.
use crate::{agent_breakdown, agent_canon, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

#[derive(Clone, Copy)]
pub enum FailureKind {
    Reference,
    Conflict,
    Permission,
    Precondition,
}
impl FailureKind {
    fn code(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Conflict => "conflict",
            Self::Permission => "permission",
            Self::Precondition => "precondition",
        }
    }
}

pub fn failure(
    code: &str,
    kind: FailureKind,
    message: &str,
    retryable: bool,
    recovery: &str,
    details: Value,
) -> Value {
    json!({"ok":false,"saved":false,"created":false,"toolError":{"code":code,"category":kind.code(),"message":message,"retryable":retryable,
        "outcome":"not_applied","recoveryTool":recovery,"details":details}})
}

fn text_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
}
fn active(value: &Value) -> bool {
    !value["deletedAt"].is_number() && !value["retiredAt"].is_number()
}

/// 可引用范围与 `agent_service::allowed_book` 的只读口径一致：本会话读过、引用已撤下的书仍在范围内。
/// 写入另由写入目标把关，不靠这里。
async fn scoped_book(db: &mut SqliteConnection, s: &Value, bid: &str) -> Result<bool, String> {
    if s["targetBookId"] == bid || crate::agent_service::read_source(s, bid) {
        return Ok(true);
    }
    if s["references"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["kind"] != "material" && r["bookId"] == bid)
    {
        return Ok(true);
    }
    if let Some(pid) = s["projectId"].as_str().filter(|s| !s.is_empty()) {
        if let Some(project) = get(db, NS, &format!("project:{pid}")).await? {
            return Ok(active(&project) && project["bookId"] == bid);
        }
    }
    Ok(false)
}

struct ReferenceScope<'a> {
    book_id: &'a str,
    material_kind: &'a str,
    session: &'a Value,
}

async fn lookup(
    db: &mut SqliteConnection,
    kind: &str,
    id: &str,
    scope: &ReferenceScope<'_>,
) -> Result<Option<Value>, String> {
    let bid = scope.book_id;
    let raw:Option<String> = match kind {
        // library_book：书库里任何未删除的作品（open_book 把书带进对话时用），不要求已在会话范围内
        "book" | "source_book" | "library_book" => {
            if let Some(pid)=agent_breakdown::project_of(id) {
                if kind!="source_book" {return Ok(None);}
                return get(db,agent_breakdown::BREAKDOWN_NS,&format!("project:{pid}")).await;
            }
            sqlx::query_scalar("SELECT payload FROM local_books WHERE CAST(id AS TEXT)=? AND deletedAt IS NULL").bind(id).fetch_optional(&mut *db).await.map_err(err)?
        }
        "chapter" | "source_chapter" => {
            if let Some(pid)=agent_breakdown::project_of(bid) {
                if kind!="source_chapter" {return Ok(None);}
                let project=get(db,agent_breakdown::BREAKDOWN_NS,&format!("project:{pid}")).await?;
                return Ok(project.as_ref().and_then(|p|p["chapters"].as_array()).into_iter().flatten().find(|c|text_id(&c["id"]).as_deref()==Some(id)).cloned());
            }
            sqlx::query_scalar("SELECT payload FROM local_chapters WHERE CAST(id AS TEXT)=? AND bookId=? AND deletedAt IS NULL").bind(id).bind(bid).fetch_optional(&mut *db).await.map_err(err)?
        }
        "volume"=>sqlx::query_scalar("SELECT payload FROM local_volumes WHERE CAST(id AS TEXT)=? AND bookId=? AND deletedAt IS NULL").bind(id).bind(bid).fetch_optional(&mut *db).await.map_err(err)?,
        "artifact"=>return get(db,NS,&format!("artifact:{id}")).await,
        "ruleset"=>return get(db,agent_canon::AUTHOR_NS,&format!("ruleset:{id}")).await,
        "skill"=>return get(db,NS,&format!("skill:{id}")).await,
        "distillation"=>return get(db,NS,&format!("distillation-job:{id}")).await,
        "breakdown"=>return get(db,agent_breakdown::BREAKDOWN_NS,&format!("project:{id}")).await,
        "material"=>return get(db,OP_NS,&crate::agent_service::material_key(id)).await,
        "profile"=>return Ok(agent_canon::platform_profiles(db).await?.into_iter().find(|p|p["id"]==id)),
        "story_thread"=>return get(db,agent_canon::CANON_NS,&format!("thread:{bid}:{id}")).await,
        "memory"=>return get(db,NS,&format!("memory:{id}")).await,
        "author_session"=>return get(db,NS,&format!("session:{id}")).await,
        "author_message"=>return Ok(scope.session["messages"].as_array().into_iter().flatten().find(|m|m["id"]==id && m["role"]=="user").cloned()),
        "material_entity"=>{
            let field=match scope.material_kind {"character"=>"characters","outline"=>"outlineNodes","world_setting"=>"worldSettings",_=>return Ok(None)};
            let materials=crate::agent_creation::materials(db,bid).await?;
            return Ok(materials[field].as_array().into_iter().flatten().find(|r|text_id(&r["id"]).as_deref()==Some(id)).cloned());
        }
        "style_example"=>return Ok(crate::agent_style::examples(db,bid,"").await?.into_iter().find(|e|e["id"]==id)),
        "edit_evidence"=>{
            let Some(e)=get(db,NS,id).await? else {return Ok(None)};
            if e["bookId"]!=bid || e["origin"]!="author_edit" {return Ok(None);}
            let Some(aid)=e["artifactId"].as_str() else {return Ok(None)};
            let current=get(db,NS,&format!("artifact:{aid}")).await?;
            return Ok(current.filter(|a|active(a) && a["revision"]==e["artifactRevision"] && a["content"]==e["after"]).map(|_|e));
        }
        _=>return Err(format!("未注册的工具资源类型：{kind}")),
    };
    raw.map(|s| serde_json::from_str(&s).map_err(err))
        .transpose()
}

fn aliases(kind: &str, value: &str, bid: &str) -> Vec<String> {
    let prefixes = match kind {
        "book" | "source_book" | "library_book" => vec!["book:".to_owned()],
        "chapter" | "source_chapter" => vec![format!("chapter:{bid}:"), "chapter:".to_owned()],
        "volume" => vec![format!("volume:{bid}:"), "volume:".to_owned()],
        "artifact" => vec!["artifact:".to_owned()],
        "ruleset" => vec!["ruleset:".to_owned()],
        "skill" => vec!["skill:".to_owned()],
        "breakdown" => vec!["breakdown:".to_owned(), "project:".to_owned()],
        "material" => vec!["material:".to_owned()],
        "profile" => vec!["profile:".to_owned()],
        "story_thread" => vec![format!("thread:{bid}:"), "thread:".to_owned()],
        "memory" => vec!["memory:".to_owned()],
        _ => vec![],
    };
    prefixes
        .iter()
        .filter_map(|prefix| value.strip_prefix(prefix))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn recovery(kind: &str) -> &'static str {
    match kind {
        "ruleset" => "resolve_ruleset",
        "artifact" => "list_artifacts",
        "library_book" => "list_books",
        "chapter" | "source_chapter" => "list_chapters",
        "volume" => "list_volumes",
        "skill" => "list_skills",
        "breakdown" => "list_breakdowns",
        "profile" => "list_writing_profiles",
        "story_thread" => "list_threads",
        "material_entity" => "read_book_materials",
        "style_example" | "edit_evidence" => "read_style_profile",
        "author_session" | "author_message" => "get_memory_sources",
        "memory" => "recall_memory",
        _ => "",
    }
}

/// Expand only declared array positions. Narrative content is never traversed as references.
fn reference_paths(value: &Value, pattern: &str) -> Vec<String> {
    let mut paths = vec![String::new()];
    for segment in pattern.trim_start_matches('/').split('/') {
        let mut next = Vec::new();
        for path in paths {
            if segment == "*" {
                if let Some(items) = value.pointer(&path).and_then(Value::as_array) {
                    next.extend((0..items.len()).map(|i| format!("{path}/{i}")));
                }
            } else {
                next.push(format!("{path}/{segment}"));
            }
        }
        paths = next;
    }
    paths
}

/// Validate before mutation. Prefer an exact public ID; only then resolve a
/// declared alias. Nested document strings/content are never rewritten.
pub async fn prepare(
    db: &mut SqliteConnection,
    s: &Value,
    name: &str,
    args: &mut Value,
) -> Result<Option<Value>, String> {
    let policy = crate::agent_contract::tool_policy(name)?;
    let mut owner = args["book_id"].as_str().unwrap_or("").to_owned();
    for reference in policy["references"].as_array().into_iter().flatten() {
        for path in reference_paths(args, str_field(reference, "path")?) {
            let Some(raw) = args
                .pointer(&path)
                .and_then(text_id)
                .filter(|v| !v.is_empty())
            else {
                continue;
            };
            let kind = str_field(reference, "resource")?;
            let mut found = None;
            for candidate in std::iter::once(raw.clone()).chain(aliases(kind, &raw, &owner)) {
                if let Some(record) = lookup(
                    db,
                    kind,
                    &candidate,
                    &ReferenceScope {
                        book_id: &owner,
                        material_kind: args
                            .pointer(
                                reference["materialKindPath"]
                                    .as_str()
                                    .unwrap_or("/material_kind"),
                            )
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        session: s,
                    },
                )
                .await?
                .filter(active)
                {
                    found = Some((candidate, record));
                    break;
                }
            }
            let Some((id, record)) = found else {
                let code = if kind == "artifact" {
                    "artifact_unavailable".to_owned()
                } else if kind == "profile" {
                    "unknown_profile".to_owned()
                } else {
                    format!("{}_not_found", kind.trim_start_matches("source_"))
                };
                return Ok(Some(failure(&code,FailureKind::Reference,"引用不存在、已失效或不属于指定作品。本次未执行，请从读取工具的结果中重新选择编号。",true,recovery(kind),json!({"field":path,"reference":raw,"resource":kind}))));
            };
            let allowed = match kind {
                "book" | "source_book" => scoped_book(db, s, &id).await?,
                "artifact" => {
                    record["sessionId"] == s["id"]
                        || match record["bookId"].as_str().filter(|v| !v.is_empty()) {
                            Some(book) => scoped_book(db, s, book).await?,
                            None => false,
                        }
                }
                "material" => crate::agent_service::referenced_material(s,&id),
                "distillation" => record["sessionId"] == s["id"],
                "author_session" => id == s["id"].as_str().unwrap_or(""),
                _ => true,
            };
            if !allowed || (reference["sameSession"] == true && record["sessionId"] != s["id"]) {
                return Ok(Some(failure(
                    if kind == "artifact" {
                        "artifact_unavailable"
                    } else {
                        "reference_out_of_scope"
                    },
                    FailureKind::Permission,
                    "引用不在当前会话的可读范围内；不能通过更换编号取得权限。",
                    false,
                    recovery(kind),
                    json!({"field":path,"resource":kind}),
                )));
            }
            if let Some(kinds) = reference["kinds"].as_array() {
                if !kinds.contains(&record["kind"]) {
                    return Ok(Some(failure(
                        "reference_kind_mismatch",
                        FailureKind::Reference,
                        "引用的成果类型与本次操作不匹配，请选择正确类型的成果。",
                        true,
                        "list_artifacts",
                        json!({"field":path,"expectedKinds":kinds,"actualKind":record["kind"]}),
                    )));
                }
            }
            if let Some(matches) = reference["matches"].as_object() {
                for (field, argument_path) in matches {
                    if args.pointer(argument_path.as_str().ok_or("资源匹配路径无效")?)
                        != record.get(field)
                    {
                        return Ok(Some(failure(
                            "reference_target_mismatch",
                            FailureKind::Reference,
                            "引用对应的目标与本次任务不一致，请读取正确目标的成果。",
                            true,
                            recovery(kind),
                            json!({"field":path,"targetField":field}),
                        )));
                    }
                }
            }
            if matches!(kind, "book" | "source_book") {
                if reference["sameOwner"] == true && !owner.is_empty() && owner != id {
                    return Ok(Some(failure(
                        "reference_owner_mismatch",
                        FailureKind::Reference,
                        "来源与指定作品不一致，本次未执行。",
                        true,
                        "get_memory_sources",
                        json!({"field":path}),
                    )));
                }
                owner = id.clone();
            }
            if matches!(kind, "artifact" | "ruleset" | "memory") {
                if let Some(book) = record["bookId"].as_str().filter(|v| !v.is_empty()) {
                    if !owner.is_empty() && owner != book {
                        return Ok(Some(failure(
                            "reference_owner_mismatch",
                            FailureKind::Reference,
                            "引用与指定作品不一致，本次未执行。",
                            true,
                            recovery(kind),
                            json!({"field":path}),
                        )));
                    }
                    owner = book.to_owned();
                }
            }
            for dependency in reference["dependencies"].as_array().into_iter().flatten() {
                let dependency_path = str_field(dependency, "path")?;
                let Some(bound) = record
                    .pointer(dependency_path)
                    .and_then(text_id)
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                let resource = str_field(dependency, "resource")?;
                let mut available = false;
                for candidate in
                    std::iter::once(bound.clone()).chain(aliases(resource, &bound, &owner))
                {
                    if lookup(
                        db,
                        resource,
                        &candidate,
                        &ReferenceScope {
                            book_id: &owner,
                            material_kind: "",
                            session: s,
                        },
                    )
                    .await?
                    .filter(active)
                    .is_some()
                    {
                        available = true;
                        break;
                    }
                }
                if !available {
                    return Ok(Some(failure(
                        &format!("{resource}_not_found"),
                        FailureKind::Reference,
                        "成果所依赖的引用已不存在或已失效，请重新读取并准备候选，保留原稿。",
                        true,
                        dependency["recoveryTool"]
                            .as_str()
                            .unwrap_or_else(|| recovery(resource)),
                        json!({"artifactId":id,"field":dependency_path}),
                    )));
                }
            }
            if reference["stableBasis"] == true {
                if let Some(fault) = crate::agent_writing_basis::check_current(db, &record).await? {
                    return Ok(Some(fault));
                }
            }
            *args.pointer_mut(&path).ok_or("资源契约路径无效")? = if reference["integer"] == true {
                json!(id.parse::<i64>().map_err(err)?)
            } else {
                json!(id)
            };
        }
    }
    Ok(None)
}

/// Bind an operation's intent before entering existing transactional domain
/// receipts. Same-stage replays may repeat; changing arguments under the same
/// operation ID cannot silently return a different request's saved result.
pub async fn bind_operation(
    db: &mut SqliteConnection,
    s: &Value,
    name: &str,
    args: &Value,
) -> Result<Option<Value>, String> {
    if crate::agent_contract::tool_policy(name)?["bindOperation"] != true {
        return Ok(None);
    }
    let Some(operation) = args["operation_id"].as_str().filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let key = format!("tool-intent:{}:{operation}", str_field(s, "id")?);
    let mut intent = args.clone();
    intent
        .as_object_mut()
        .ok_or("工具参数不是对象")?
        .remove("operation_id");
    let fingerprint = crate::agent_creation::version(&json!([name, intent]));
    let record = json!({"tool":name,"operationId":operation,"fingerprint":fingerprint});
    sqlx::query("INSERT INTO desktop_records(namespace,key,value) VALUES(?,?,?) ON CONFLICT(namespace,key) DO NOTHING")
        .bind(OP_NS).bind(json!(key).to_string()).bind(record.to_string()).execute(&mut *db).await.map_err(err)?;
    let old = get(db, OP_NS, &key).await?.ok_or("操作意图记录缺失")?;
    if old["fingerprint"] != fingerprint {
        return Ok(Some(failure("operation_intent_mismatch",FailureKind::Conflict,"同一操作编号已绑定另一组参数，不能复用旧结果或覆盖原操作。请先读取已有成果；确需新的操作时使用新的工具调用。",false,"list_artifacts",json!({"tool":name}))));
    }
    Ok(None)
}

/// Model-facing numeric database identities follow the declared string input
/// contract. The stored records and editor-facing commands retain their types.
pub fn public_result(name: &str, mut value: Value) -> Value {
    let stringify = |v: &mut Value, fields: &[&str]| {
        for field in fields {
            if v[*field].is_number() {
                v[*field] = json!(v[*field].to_string());
            }
        }
    };
    match name {
        "list_chapters" | "list_volumes" => {
            for v in value.as_array_mut().into_iter().flatten() {
                stringify(v, &["id", "bookId", "volumeId"]);
            }
        }
        "read_chapter" | "create_chapter" | "create_volume" => {
            stringify(&mut value, &["id", "chapterId", "bookId", "volumeId"])
        }
        "load_book_context" => {
            if value["book"].is_object() {
                stringify(&mut value["book"], &["id"]);
            }
        }
        _ => {}
    }
    if name == "list_threads" {
        for item in value["threads"].as_array_mut().into_iter().flatten() {
            if item["threadId"].is_string() {
                if let Some(fields) = item.as_object_mut() {
                    fields.remove("id");
                }
            }
        }
    }
    value
}
