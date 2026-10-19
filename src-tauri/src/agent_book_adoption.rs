//! 对话采用与界面采用共用事务。自然语言解释在运行时完成，宿主绑定作者原话、已展示版本及一次性回执。
use crate::{agent_book_plan, agent_creation, agent_store::*, agent_tool_boundary::{failure, FailureKind}};
use serde_json::{json, Value};
use std::path::Path;

pub fn fingerprint(plan: &Value) -> String {
    agent_creation::version(&json!([plan["revision"], plan["bookPlan"]]))
}

fn confirmation(token: &str, plan: &Value) -> Value {
    json!({"status":"confirmation_required","confirmation":{"token":token,
        "question":format!("把《{}》建成作品吗？", plan["bookPlan"]["workingTitle"].as_str().unwrap_or("未定名")),
        "choices":["创建作品","先不创建"]}})
}

fn result(plan: &Value) -> Value {
    json!({"saved":true,"created":plan["planBase"].is_null(),"artifact_id":plan["id"],"bookId":plan["bookId"],"title":plan["bookTitle"],"revision":plan["revision"]})
}

pub async fn begin(root: &Path, sid: &str, run: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let s = session(&mut tx, sid).await?;
    if s["runId"] != run
        || s["status"] != "running"
        || s["pendingInputs"].as_array().is_some_and(|v| !v.is_empty())
    {
        return Err("作者要求或任务状态已变化，请重新核对".into());
    }
    let aid = str_field(args, "artifact_id")?;
    // 模型能自行纠正的前置条件（引用、版本、还没定的作者决定）以结构化失败退回，交回模型处理，不当成宿主故障中断运行；
    // 授权凭据类错误（作者原话、回执、展示版本）仍是硬错误。
    let Some(plan) = get(&mut tx, NS, &format!("artifact:{aid}"))
        .await?
        .filter(|plan| plan["kind"] == "book_plan" && plan["sessionId"] == sid)
    else {
        return Ok(failure("plan_not_found", FailureKind::Reference,
            "当前会话里没有这份开书方案；用 read_artifact 取得编号与版本后再创建作品。", false, "read_artifact", json!({})));
    };
    if plan["applied"] == true {
        book(&mut tx, str_field(&plan, "bookId")?).await?;
        return Ok(result(&plan));
    }
    let revision = args["expected_revision"].as_u64().ok_or("缺少草案版本")?;
    if plan["revision"] != revision {
        return Ok(failure("plan_revision_stale", FailureKind::Conflict,
            "开书方案已有新版本；读取当前版本后再创建作品。", false, "read_artifact", json!({"revision":plan["revision"]})));
    }
    let existing_book = plan["bookId"].as_str().is_some_and(|b| !b.is_empty());
    let readiness = agent_book_plan::progress(&plan["bookPlan"], false, existing_book);
    if readiness["buildable"] != true {
        let blocking = readiness["blocking"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("、");
        return Ok(failure("plan_incomplete", FailureKind::Precondition,
            &format!("创建作品还缺少：{blocking}。按 progress.current 回到尚未完成的步骤，沿用作者已回答的内容；尚未决定的请作者选择或明确委托拟定，再用 prepare_book_plan 更新当前方案。不要把 AI 建议当作作者决定。"),
            false, "prepare_book_plan", json!({"planId":aid,"revision":revision,"blocking":readiness["blocking"],"current":readiness["current"]})));
    }
    let key = format!("book-plan-consent:{sid}:{aid}");
    let digest = fingerprint(&plan);
    if let Some(previous) = get(&mut tx, OP_NS, &key)
        .await?
        .filter(|v| v["fingerprint"] == digest && v["consumed"] != true)
    {
        if let Some((choice, reply)) =
            crate::agent_prompt::confirmation_answer(&s, previous["token"].as_str().unwrap_or(""))
        {
            if choice.as_deref() == Some("0") {
                let adopted = agent_book_plan::adopt(&mut tx, aid, revision).await?;
                tx.commit().await.map_err(err)?;
                return Ok(result(&adopted));
            }
            if choice.is_some() {
                let mut declined = previous.clone();
                declined["consumed"] = json!(true);
                put(&mut tx, OP_NS, &key, &declined).await?;
                tx.commit().await.map_err(err)?;
                return Ok(json!({"saved":false,"status":"declined","authorReply":reply}));
            }
        }
    }
    let author = s["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m["role"] == "user" && m["runId"] == run && m["source"] != "batch");
    let reviewed = s["reviewablePlans"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| p["id"] == aid && p["fingerprint"] == digest);
    let nonce = id();
    let record = json!({"nonce":nonce,"token":nonce,"artifactId":aid,"revision":revision,"fingerprint":digest,
        "runId":run,"canInterpret":reviewed && author.is_some(),"sourceMessageId":author.map(|m| m["id"].clone()),"sourceText":author.map(|m| m["text"].clone()),"consumed":false});
    put(&mut tx, OP_NS, &key, &record).await?;
    tx.commit().await.map_err(err)?;
    if !reviewed || author.is_none() {
        return Ok(confirmation(&nonce, &plan));
    }
    let messages = s["messages"].as_array().cloned().unwrap_or_default();
    let previous_reply = messages.iter().rev()
        .find(|m| m["role"] == "assistant" && m["runId"] != run && m["kind"].is_null());
    // Preserve the preceding exchange as a whole: a short answer inherits the
    // action being discussed, not a hardcoded meaning assigned to its wording.
    let preceding = messages.iter().filter(|m| {
        previous_reply.is_some_and(|reply| m["runId"] == reply["runId"])
            && matches!(m["role"].as_str(), Some("user") | Some("assistant"))
            && m["text"].as_str().is_some_and(|text| !text.is_empty())
    }).map(|m| json!({"role":m["role"],"text":m["text"]})).collect::<Vec<_>>();
    Ok(
        json!({"status":"interpret_author","nonce":nonce,"authorText":record["sourceText"],
        "plan":{"id":aid,"revision":revision,"title":plan["bookPlan"]["workingTitle"]},
        "otherPlans":s["reviewablePlans"],"precedingConversation":preceding,
        "previousReply":previous_reply.map(|m|m["text"].clone())}),
    )
}

/// 仅运行时的授权解释器调用，不作为模型可直接调用的工具暴露。
pub async fn finish(root: &Path, sid: &str, run: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let s = session(&mut tx, sid).await?;
    if s["runId"] != run
        || s["status"] != "running"
        || s["pendingInputs"].as_array().is_some_and(|v| !v.is_empty())
    {
        return Err("任务已变化，本次未采用".into());
    }
    let aid = str_field(args, "artifact_id")?;
    let key = format!("book-plan-consent:{sid}:{aid}");
    let mut consent = get(&mut tx, OP_NS, &key).await?.ok_or("采用上下文已失效")?;
    if consent["nonce"] != args["nonce"] || consent["runId"] != run || consent["consumed"] == true {
        return Err("采用凭据已失效".into());
    }
    let author = s["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["id"] == consent["sourceMessageId"] && m["role"] == "user")
        .ok_or("作者原话不存在")?;
    if author["text"] != consent["sourceText"] {
        return Err("作者原话已变化".into());
    }
    let plan = get(&mut tx, NS, &format!("artifact:{aid}"))
        .await?
        .ok_or("草案不存在")?;
    if fingerprint(&plan) != consent["fingerprint"] || plan["sessionId"] != sid {
        return Err("草案已被修改，不能用旧授权采用新版".into());
    }
    let decision = str_field(args, "decision")?;
    if decision == "unclear" {
        return Ok(confirmation(str_field(&consent, "token")?, &plan));
    }
    consent["consumed"] = json!(true);
    put(&mut tx, OP_NS, &key, &consent).await?;
    let output = if decision == "adopt" {
        if consent["canInterpret"] != true {
            return Err("该版本未在本轮作者指令前展示，不能通过文字解释入口采用".into());
        }
        if args["author_quote"] != author["text"] {
            return Err("采用依据必须对应完整作者原话".into());
        }
        result(
            &agent_book_plan::adopt(
                &mut tx,
                aid,
                consent["revision"].as_u64().ok_or("草案版本缺失")?,
            )
            .await?,
        )
    } else if decision == "decline" {
        json!({"saved":false,"status":"not_authorized","message":"作者没有授权采用当前草案，未建立或修改作品。"})
    } else {
        return Err("采用决定无效".into());
    };
    tx.commit().await.map_err(err)?;
    Ok(output)
}
