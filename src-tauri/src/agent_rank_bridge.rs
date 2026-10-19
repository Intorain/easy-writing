//! 榜单工具：把 Agent 的榜单领域调用经网页桥交给应用级 TS 服务，复用现有 HTTP/WebView 抓取适配。
//! 这里只剩榜单自己的规则：大范围联网抓取前的作者确认、同一范围的确认沿用、只复用成功结果的回执。
//! 传输、执行器注册与回传在 agent_webview_bridge。
use crate::{agent_store::*, agent_webview_bridge as webview, DesktopAppHandle};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::Duration,
};

/// 单个来源的最坏耗时：起点 5 页 × 隐藏窗口 60 秒上限，再留字体下载与同源排队的余量。
/// 单榜请求以它为总时限；范围采集以它为两次进度之间的最长间隔。
const SOURCE_BUDGET: Duration = Duration::from_secs(420);
const COLLECTION_DEADLINE: Duration = Duration::from_secs(3600);
/// 作者同意大范围联网抓取后，同一范围在这段时间内不再重复确认。
const CONFIRM_TTL_MS: i64 = 30 * 60 * 1000;
/// 联网确认的选项编号即界面中的位置。默认选中的第 0 项是只读本地快照，未作答的恢复不会变成联网。
const CHOICE_CACHED: &str = "0";
const CHOICE_FETCH: &str = "1";
const CHOICE_CANCEL: &str = "2";

fn unavailable(code: &str, message: &str) -> Value {
    let mut value = webview::unavailable(code, message);
    value["freshnessSatisfied"] = json!(false);
    value
}

/// 确认记录按采集范围比对；展示数量不改变要抓取的来源，不参与比较。
fn collection_scope(request: &Value) -> Value {
    json!({"siteCode":request["siteCode"],"rankType":request["rankType"],"gender":request["gender"],
        "category":request["category"],"freshness":request["freshness"].as_str().unwrap_or("latest"),
        "statDate":request["statDate"].as_str().unwrap_or("")})
}

/// 作者对联网确认的回应，由宿主从会话记录里读出，不来自模型参数或模型转述。
#[derive(Debug, PartialEq)]
enum Consent {
    /// 没有待处理的确认：按常规执行，需要时发出新的确认
    None,
    Approved,
    CachedOnly,
    Declined,
    /// 作者打字回复或未作答就恢复：不算同意，原话交给模型
    Unanswered(String),
}

impl Consent {
    fn label(&self) -> &'static str {
        match self {
            Consent::None => "",
            Consent::Approved => "approved",
            Consent::CachedOnly => "cached_only",
            Consent::Declined => "cancelled",
            Consent::Unanswered(_) => "unanswered",
        }
    }
}

/// 找到作者对这次确认的回应。只认点选：确认消息已作答、选中项存在，且其后作者消息正是该选项文字
/// （界面点选时两者一致）。打字回复、未作答就恢复都返回 None 作为选中项，只带上作者原话。
/// 确认记录只对同一采集范围有效。首次读到作者点选后结论写回记录：同意在有效期内沿用，其他结论只用一次。
fn decide(record: Option<&Value>, session: &Value, scope: &Value, now_ms: i64) -> Consent {
    let Some(record) = record.filter(|r| r["scope"] == *scope) else {
        return Consent::None;
    };
    match record["decision"].as_str() {
        Some("approved")
            if now_ms - record["decidedAt"].as_i64().unwrap_or(0) < CONFIRM_TTL_MS =>
        {
            Consent::Approved
        }
        Some(_) => Consent::None,
        None => match crate::agent_prompt::confirmation_answer(session, record["token"].as_str().unwrap_or("")) {
            None => Consent::None,
            Some((Some(id), _)) if id == CHOICE_FETCH => Consent::Approved,
            Some((Some(id), _)) if id == CHOICE_CACHED => Consent::CachedOnly,
            Some((Some(id), _)) if id == CHOICE_CANCEL => Consent::Declined,
            Some((_, reply)) => Consent::Unanswered(reply),
        },
    }
}

/// 确认问题与选项由宿主拼出，顺序与 CHOICE_* 对应；工具原样交给作者。
fn confirmation_prompt(confirmation: &Value) -> (String, Vec<String>) {
    let pending = confirmation["pendingFetches"].as_u64().unwrap_or(0);
    let fresh = confirmation["alreadyFresh"].as_u64().unwrap_or(0);
    let minutes = confirmation["estimatedMinutes"].as_u64().unwrap_or(1);
    let scope = confirmation["scopeLabel"].as_str().unwrap_or("这个范围");
    let fresh_note = if fresh > 0 {
        format!("（另有 {fresh} 个已是今天的数据）")
    } else {
        String::new()
    };
    (
        format!("汇总{scope}需要联网抓取 {pending} 个榜单，预计约 {minutes} 分钟{fresh_note}。要现在抓取吗？"),
        vec![
            "先只用本地已有快照".to_owned(),
            format!("联网抓取这 {pending} 个榜单（约 {minutes} 分钟）"),
            "取消这次汇总".to_owned(),
        ],
    )
}

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "list_rank_sources" | "get_rank" | "get_rank_collection"
    )
}

pub fn execute(
    app: &DesktopAppHandle,
    root: &Path,
    sid: &str,
    run: &str,
    name: &str,
    args: &Value,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    let args = crate::agent_contract::canonical_arguments(args);
    if let Some(error) = crate::agent_contract::tool_error(name, &args)? {
        return Ok(error);
    }
    let operation_id = str_field(&args, "operation_id")?;
    let receipt_key = format!("rank:{sid}:{run}:{operation_id}");
    let collection = name == "get_rank_collection";
    let confirm_key = format!("rank-confirm:{sid}");
    let scope = collection_scope(&args["request"]);
    let (existing, consent) = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let s = session(&mut db, sid).await?;
        if s["runId"] != run || s["status"] != "running" {
            return Err("榜单任务已停止或失效".to_string());
        }
        let existing = get(&mut db, OP_NS, &receipt_key).await?;
        let mut consent = Consent::None;
        if collection && existing.is_none() {
            let record = get(&mut db, OP_NS, &confirm_key).await?;
            consent = decide(record.as_ref(), &s, &scope, now());
            if let Some(mut record) =
                record.filter(|r| r["decision"].is_null() && consent != Consent::None)
            {
                record["decision"] = json!(consent.label());
                record["decidedAt"] = json!(now());
                put(&mut db, OP_NS, &confirm_key, &record).await?;
            }
        }
        Ok::<_, String>((existing, consent))
    })?;
    if let Some(receipt) = existing {
        if receipt["tool"] != name || receipt["request"] != args["request"] {
            return Err("重复操作编号对应不同榜单请求".into());
        }
        return Ok(receipt["result"].clone());
    }
    match &consent {
        Consent::Declined => {
            return Ok(json!({"status":"declined","freshnessSatisfied":false,"authorDecision":"cancelled",
                "message":"作者在确认中选择取消，本次没有联网抓取。不要再次发起同一范围的抓取，除非作者重新提出。"}))
        }
        Consent::Unanswered(reply) => {
            return Ok(json!({"status":"not_confirmed","freshnessSatisfied":false,"authorReply":reply,
                "message":"作者没有在确认选项里点选联网抓取，本次没有抓取。按作者的回复继续；作者明确要求抓取时，再调用本工具会重新发出确认。"}))
        }
        _ => {}
    }
    // 作者选择只读本地时，宿主把本次请求改为 cached；networkApproved 只在作者点选抓取时为真。
    let mut request = args["request"].clone();
    if consent == Consent::CachedOnly {
        request["freshness"] = json!("cached");
        if let Some(fields) = request.as_object_mut() {
            fields.remove("statDate");
        }
    }
    let approved = consent == Consent::Approved;
    webview::report(app, root, sid, run, &json!({"event":"tool_started","name":name}));
    let budget = webview::Budget {
        deadline: if collection { COLLECTION_DEADLINE } else { SOURCE_BUDGET },
        idle: SOURCE_BUDGET,
    };
    let mut result = webview::request(app, name, request, json!({"networkApproved":approved}), budget, cancelled, |completed, total| {
        if collection {
            webview::report(
                app,
                root,
                sid,
                run,
                &json!({"event":"tool_progress","name":name,"completed":completed,"total":total}),
            );
        }
    })?;
    if result["error"].is_object() && result["freshnessSatisfied"].is_null() {
        // 执行器层面的失败（未就绪、超时、取消）补上榜单结果的时效字段
        result["freshnessSatisfied"] = json!(false);
    }
    if matches!(result["status"].as_str(), Some("cancelled" | "executor_timeout")) {
        return Ok(unavailable(result["status"].as_str().unwrap_or("cancelled"), "榜单请求未完成"));
    }
    if consent == Consent::CachedOnly {
        result["authorDecision"] = json!("cached_only");
    }
    // 需要作者确认时由宿主生成确认编号、问题与选项；工具据此向作者发起点选，宿主之后按点选结果放行。
    let issued = if collection && result["status"] == "confirmation_required" {
        let token = id();
        let (question, choices) = confirmation_prompt(&result["confirmation"]);
        result["confirmation"]["token"] = json!(token);
        result["confirmation"]["question"] = json!(question);
        result["confirmation"]["choices"] = json!(choices);
        Some(json!({"scope":scope,"token":token,"runId":run,"issuedAt":now()}))
    } else {
        None
    };
    crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let s = session(&mut tx, sid).await?;
        if s["runId"] != run || s["status"] != "running" {
            return Err("榜单任务已停止或失效".into());
        }
        if let Some(record) = &issued {
            put(&mut tx, OP_NS, &confirm_key, record).await?;
        }
        // 只复用成功的同次工具结果；临时失败允许框架再次执行。
        if result["status"] == "available" {
            put(
                &mut tx,
                OP_NS,
                &receipt_key,
                &json!({"tool":name,"request":args["request"],"result":result}),
            )
            .await?;
        }
        tx.commit().await.map_err(err)
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(selected: Value, reply: Option<&str>) -> Value {
        let (question, choices) = confirmation_prompt(
            &json!({"pendingFetches":37,"alreadyFresh":0,"estimatedMinutes":13,"scopeLabel":"番茄小说网阅读榜"}),
        );
        let options = choices
            .iter()
            .enumerate()
            .map(|(i, title)| json!({"id":i.to_string(),"title":title}))
            .collect::<Vec<_>>();
        let mut messages = vec![json!({"id":"run-1:choice","runId":"run-1","role":"assistant","text":question,"kind":"choice",
            "choice":{"status":"selected","selectedId":selected,"confirmation":"token-1","options":options}})];
        if let Some(text) = reply {
            messages.push(json!({"runId":"run-2","role":"user","text":text}));
        }
        json!({ "messages": messages })
    }

    #[test]
    fn only_an_explicit_fetch_pick_approves_network_fetches_for_the_same_scope() {
        let request = json!({"siteCode":"fanqie","rankType":"reading","limit":5});
        let scope = collection_scope(&request);
        // 展示数量与默认时效不改变范围
        assert_eq!(
            scope,
            collection_scope(&json!({"siteCode":"fanqie","rankType":"reading","freshness":"latest","perCategoryLimit":2}))
        );
        let (_, choices) = confirmation_prompt(&json!({"pendingFetches":37,"estimatedMinutes":13}));
        assert!(choices[1].contains("37") && choices[0].contains("本地"));
        let record = json!({"scope":scope,"token":"token-1","runId":"run-1","issuedAt":1_000});
        let fetch = choices[1].as_str();
        assert_eq!(decide(None, &answered(json!("1"), Some(fetch)), &scope, 2_000), Consent::None);
        assert_eq!(decide(Some(&record), &answered(json!("1"), Some(fetch)), &scope, 2_000), Consent::Approved);
        assert_eq!(decide(Some(&record), &answered(json!("0"), Some(choices[0].as_str())), &scope, 2_000), Consent::CachedOnly);
        assert_eq!(decide(Some(&record), &answered(json!("2"), Some(choices[2].as_str())), &scope, 2_000), Consent::Declined);
        // 打字回复“取消”或“同意”都不算点选
        assert_eq!(decide(Some(&record), &answered(Value::Null, Some("取消")), &scope, 2_000), Consent::Unanswered("取消".into()));
        assert_eq!(decide(Some(&record), &answered(Value::Null, Some("同意")), &scope, 2_000), Consent::Unanswered("同意".into()));
        // 未作答就恢复：带着界面默认选中项也没有对应的作者消息，不算点选
        assert_eq!(decide(Some(&record), &answered(json!("1"), None), &scope, 2_000), Consent::Unanswered(String::new()));
        // 选中编号与作者消息不一致（例如被改写的选项）不算点选
        assert_eq!(decide(Some(&record), &answered(json!("1"), Some("随便")), &scope, 2_000), Consent::Unanswered("随便".into()));
        // 确认还没作答：不放行
        let mut pending = answered(json!("1"), Some(fetch));
        pending["messages"][0]["choice"]["status"] = json!("pending");
        assert_eq!(decide(Some(&record), &pending, &scope, 2_000), Consent::None);
        // 范围不同要重新确认
        let female = collection_scope(&json!({"siteCode":"fanqie","rankType":"reading","gender":"female"}));
        assert_eq!(decide(Some(&record), &answered(json!("1"), Some(fetch)), &female, 2_000), Consent::None);
        // 同意写回记录后在有效期内沿用；其他结论只用一次
        let approved = json!({"scope":scope,"token":"token-1","decision":"approved","decidedAt":1_000});
        assert_eq!(decide(Some(&approved), &json!({"messages":[]}), &scope, 2_000), Consent::Approved);
        assert_eq!(decide(Some(&approved), &json!({"messages":[]}), &scope, 1_000 + CONFIRM_TTL_MS), Consent::None);
        let used = json!({"scope":scope,"token":"token-1","decision":"cancelled","decidedAt":1_000});
        assert_eq!(decide(Some(&used), &answered(json!("2"), Some(choices[2].as_str())), &scope, 2_000), Consent::None);
        assert!(handles("get_rank_collection") && !handles("send_rank_chart"));
    }
}
