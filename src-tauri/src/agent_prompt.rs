//! 引导交互：模型发出的引导卡（AuthorPrompt）规范化为界面能渲染、作者一定能作答的卡片；
//! 作者提交的结构化答案（AuthorAnswer）在这里校验，再原样交还模型。
//!
//! 旧写法（question + choices）与宿主发起的联网确认一律升格为单选卡。对话里记下的作者消息仍是
//! 所选选项的标题，依赖“点选标题 = 回答文字”的确认逻辑（如榜单联网确认）不受影响。
use crate::agent_contract::{canonical_arguments, validate_document};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The composer, a card submission and an empty resume are distinct user actions.
#[derive(PartialEq, Eq)]
pub enum RunInput { Message, Answer, Continue }

pub fn run_input(session: &Value, args: &Value) -> Result<RunInput, String> {
    match args["inputKind"].as_str() {
        Some("message") => Ok(RunInput::Message),
        Some("answer") => Ok(RunInput::Answer),
        Some("continue") => Ok(RunInput::Continue),
        Some(_) => Err("未知的对话输入类型".into()),
        // Compatibility with saved clients: explicit answers retain their old
        // path; plain composer text can never become an implicit form answer.
        None if args["answer"].is_object() || args["choiceId"].is_string() => Ok(RunInput::Answer),
        None if args["resume"] == true && session["status"] == "choice"
            && args["text"].as_str().is_some_and(|s| !s.trim().is_empty()) => Ok(RunInput::Message),
        None if args["resume"] == true => Ok(RunInput::Continue),
        None => Ok(RunInput::Message),
    }
}

/// Supersession records no answer and grants no permission. Answers submitted
/// to a parallel card but not yet delivered also belong to the abandoned wait.
pub fn supersede_questions(session: &mut Value) {
    let waiting_run = session["runId"].clone();
    for message in session["messages"].as_array_mut().into_iter().flatten() {
        if !message["choice"].is_object() { continue; }
        let same_run = message["runId"] == waiting_run;
        let choice = &mut message["choice"];
        if choice["status"] == "pending" || same_run && choice["status"] == "selected" && choice["delivered"] != true {
            choice["status"] = json!("superseded");
        }
    }
    session["choiceId"] = Value::Null;
    session["promptDraft"] = Value::Null;
}

/// 宿主确认的真实答复：只认已提交选项及对应作者消息，默认选中、自由文字和模型转述都不算点选。
pub fn confirmation_answer(session: &Value, token: &str) -> Option<(Option<String>, String)> {
    let messages = session["messages"].as_array()?;
    let index = messages
        .iter()
        .position(|m| m["choice"]["confirmation"] == token)?;
    let choice = &messages[index]["choice"];
    if choice["status"] != "selected" {
        return None;
    }
    // 回答记在卡上（几张卡一起答时作者消息是合并的）；旧会话退回卡片之后的第一条作者消息
    let reply = match choice["answerText"].as_str() {
        Some(text) => text.to_owned(),
        None => messages[index + 1..]
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["text"].as_str())
            .unwrap_or("")
            .to_owned(),
    };
    let picked = choice["selectedId"]
        .as_str()
        .filter(|id| {
            choice["options"].as_array().is_some_and(|options| {
                options
                    .iter()
                    .any(|o| o["id"] == *id && o["title"] == reply)
            })
        })
        .map(str::to_owned);
    Some((picked, reply))
}

/// 只能选一项的卡片类型。
fn single_select(kind: &str) -> bool {
    matches!(kind, "choice" | "compare" | "confirm")
}

fn legacy(question: &str, choices: &[Value]) -> Value {
    let options = choices
        .iter()
        .filter_map(Value::as_str)
        .enumerate()
        .map(|(i, title)| json!({"id":i.to_string(),"title":title}))
        .collect::<Vec<_>>();
    json!({"kind":"choice","question":if question.trim().is_empty() { "请选择" } else { question },"options":options,"allowFreeText":true})
}

/// waiting 事件 → 规范化的引导卡。`artifact_titles` 用来给没写选项的并排对比卡生成“采用某成果”选项。
pub fn normalize(event: &Value, artifact_titles: &BTreeMap<String, String>) -> Value {
    let Some(raw) = event.get("prompt").filter(|p| p.is_object()) else {
        let choices = event["choices"].as_array().cloned().unwrap_or_default();
        return legacy(event["question"].as_str().unwrap_or(""), &choices);
    };
    let mut prompt = canonical_arguments(raw);
    if validate_document("authorPrompt", &prompt).is_err() {
        // 契约不合法时保留问题和能读出的选项，不让作者卡在无法作答的卡片上
        let titles = prompt["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["title"].as_str().map(|t| json!(t)))
            .collect::<Vec<_>>();
        return legacy(prompt["question"].as_str().unwrap_or(""), &titles);
    }
    let kind = prompt["kind"].as_str().unwrap_or("choice").to_owned();
    // 选项编号去重，保持模型给的顺序
    let mut seen = BTreeSet::new();
    let mut options = prompt["options"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|o| {
            o["id"]
                .as_str()
                .is_some_and(|id| seen.insert(id.to_owned()))
        })
        .collect::<Vec<_>>();
    if options.is_empty() && kind == "confirm" {
        options = vec![
            json!({"id":"yes","title":"确认"}),
            json!({"id":"no","title":"先不要"}),
        ];
    }
    if options.is_empty() && kind == "compare" {
        options = prompt["artifactIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|id| json!({"id":id,"title":format!("采用「{}」", artifact_titles.get(id).map_or("这份成果", String::as_str))}))
            .collect();
    }
    prompt["options"] = json!(options);
    if kind == "steps" {
        // 分步问答：问题编号去重、每问的选项编号去重；一问都不剩就退回单选卡，作者至少能打字回答
        let steps = normalize_steps(&prompt["steps"]);
        if steps.is_empty() {
            return legacy(prompt["question"].as_str().unwrap_or(""), &[]);
        }
        prompt["steps"] = json!(steps);
    }
    let has_fields = prompt["fields"].as_array().is_some_and(|f| !f.is_empty());
    if options.is_empty() && !has_fields && kind != "steps" {
        // 没有可点的选项也没有表单：作者只能打字回答
        prompt["allowFreeText"] = json!(true);
    }
    if let Some(default) = prompt["defaultOptionId"].as_str() {
        if !options.iter().any(|o| o["id"] == default) {
            prompt.as_object_mut().unwrap().remove("defaultOptionId");
        }
    }
    prompt
}

fn normalize_steps(raw: &Value) -> Vec<Value> {
    let mut keys = BTreeSet::new();
    raw.as_array()
        .into_iter()
        .flatten()
        .filter(|step| step["question"].as_str().is_some_and(|q| !q.trim().is_empty()))
        .filter(|step| step["key"].as_str().is_some_and(|k| keys.insert(k.to_owned())))
        .map(|step| {
            let mut step = step.clone();
            let mut ids = BTreeSet::new();
            let options = step["options"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|o| o["id"].as_str().is_some_and(|id| ids.insert(id.to_owned())))
                .collect::<Vec<_>>();
            step["options"] = json!(options);
            step
        })
        .collect()
}

/// 一问的回答写成一行给作者看：所选标题、自己写的话。
fn step_line(question: &str, titles: &[String], text: &str) -> String {
    let question = question.trim().trim_end_matches(['？', '?', '：', ':']);
    let mut answer = titles.join("、");
    if !text.is_empty() {
        if !answer.is_empty() {
            answer.push_str("；");
        }
        answer.push_str(text);
    }
    format!("{question}：{answer}")
}

/// 分步问答的回答：逐问校验选项与必填，整理成（交给模型的逐问答案，留空的问题，对话里的行）。
fn check_steps(prompt: &Value, answer: &Value) -> Result<(Vec<Value>, Vec<String>, Vec<String>), String> {
    let (mut answers, mut blank, mut lines) = (Vec::new(), Vec::new(), Vec::new());
    for step in prompt["steps"].as_array().into_iter().flatten() {
        let key = step["key"].as_str().unwrap_or("");
        let question = step["question"].as_str().unwrap_or(key);
        let given = &answer["steps"][key];
        let options = step["options"].as_array().cloned().unwrap_or_default();
        let mut ids: Vec<String> = Vec::new();
        for id in given["selectedIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !options.iter().any(|o| o["id"] == id) {
                return Err(format!("「{question}」的所选项不在给出的选项里，请重新选择"));
            }
            if !ids.iter().any(|seen| seen == id) {
                ids.push(id.to_owned());
            }
        }
        if step["multiple"] != true && ids.len() > 1 {
            return Err(format!("「{question}」只能选一项"));
        }
        let text = given["text"].as_str().unwrap_or("").trim().to_owned();
        if ids.is_empty() && text.is_empty() {
            if step["required"] == true {
                return Err(format!("请回答「{question}」"));
            }
            blank.push(question.to_owned());
            continue;
        }
        let titles = ids
            .iter()
            .filter_map(|id| options.iter().find(|o| o["id"] == id.as_str()))
            .map(|o| o["title"].as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>();
        lines.push(step_line(question, &titles, &text));
        let mut item = json!({"key":key,"question":question,"selectedIds":ids,"selected":titles});
        if !text.is_empty() {
            item["text"] = json!(text);
        }
        answers.push(item);
    }
    Ok((answers, blank, lines))
}

/// 旧会话里只有 `choice` 字段的待答问题，升格成单选卡。
pub fn from_message(message: &Value) -> Value {
    if message["prompt"].is_object() {
        return message["prompt"].clone();
    }
    let options = message["choice"]["options"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| json!({"id":o["id"],"title":o["title"]}))
        .collect::<Vec<_>>();
    json!({"kind":"choice","question":message["text"],"options":options,"allowFreeText":true})
}

/// 默认选中项：模型指定的，否则单选类卡片的第一项。确认卡（写入、删除、花钱的操作）不预选，
/// 作者必须亲手点"确认"或"取消"。
pub fn default_choice(prompt: &Value) -> Value {
    if let Some(id) = prompt["defaultOptionId"].as_str() {
        return json!(id);
    }
    if prompt["kind"] == "confirm" {
        return json!("");
    }
    if single_select(prompt["kind"].as_str().unwrap_or("choice")) {
        if let Some(first) = prompt["options"].as_array().and_then(|o| o.first()) {
            return first["id"].clone();
        }
    }
    json!("")
}

/// 校验作者的答案。返回（交给模型的答案，写进对话的作者消息）。
/// 选中项必须属于卡片、单选类最多一项、表单必填项不能空；卡片没声明的表单字段丢弃。
pub fn check_answer(prompt: &Value, answer: &Value) -> Result<(Value, String), String> {
    let answer = canonical_arguments(answer);
    validate_document("authorAnswer", &answer)?;
    let kind = prompt["kind"].as_str().unwrap_or("choice");
    let options = prompt["options"].as_array().cloned().unwrap_or_default();
    let mut ids: Vec<String> = Vec::new();
    for id in answer["selectedIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !options.iter().any(|o| o["id"] == id) {
            return Err("所选项不在这个问题的选项里，请重新选择".into());
        }
        if !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_owned());
        }
    }
    if single_select(kind) && ids.len() > 1 {
        return Err("这个问题只能选一项".into());
    }
    let titles = ids
        .iter()
        .filter_map(|id| options.iter().find(|o| o["id"] == id.as_str()))
        .map(|o| o["title"].as_str().unwrap_or("").to_owned())
        .collect::<Vec<_>>();
    let mut fields = Map::new();
    let mut field_lines = Vec::new();
    // 表单或分步问答里作者留空、跳过的选填项：不是漏填，是交给 Agent 先给建议
    let mut blank = Vec::new();
    let (step_answers, step_blank, step_lines) = if kind == "steps" { check_steps(prompt, &answer)? } else { Default::default() };
    blank.extend(step_blank);
    field_lines.extend(step_lines);
    for field in prompt["fields"].as_array().into_iter().flatten() {
        let key = field["key"].as_str().unwrap_or("");
        let label = field["label"].as_str().unwrap_or(key);
        let value = answer["fields"][key]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        if value.is_empty() {
            if field["required"] == true && kind == "form" {
                return Err(format!("请填写「{label}」"));
            }
            blank.push(label.to_owned());
            continue;
        }
        if field["type"] == "select"
            && field["options"]
                .as_array()
                .is_some_and(|o| !o.is_empty() && !o.iter().any(|v| v == value.as_str()))
        {
            return Err(format!("「{label}」只能从给出的选项里选"));
        }
        if field["type"] == "number" && value.parse::<f64>().is_err() {
            return Err(format!("「{label}」需要填数字"));
        }
        field_lines.push(format!("{label}：{value}"));
        fields.insert(key.to_owned(), json!(value));
    }
    let text = answer["text"].as_str().unwrap_or("").trim().to_owned();
    let form = kind == "form" || kind == "steps";
    // 选择类卡片空着提交多半是误点；表单与分步问答的选填项全空是“都还没想好”，照样可以交
    if !form && ids.is_empty() && fields.is_empty() && text.is_empty() {
        return Err("请先选择或填写后再提交".into());
    }
    let mut parts = Vec::new();
    if !titles.is_empty() {
        parts.push(titles.join("、"));
    }
    parts.extend(field_lines);
    if form && !blank.is_empty() {
        parts.push(if fields.is_empty() && step_answers.is_empty() {
            "这几项我还没想好，你先给建议".to_owned()
        } else {
            format!("{}的你先给建议：{}", if kind == "steps" { "跳过" } else { "没填" }, blank.join("、"))
        });
    }
    if !text.is_empty() {
        parts.push(text.clone());
    }
    let mut normalized = json!({"selectedIds":ids,"selected":titles});
    if form && !blank.is_empty() {
        normalized["blank"] = json!(blank);
    }
    if !step_answers.is_empty() {
        normalized["answers"] = json!(step_answers);
    }
    if !text.is_empty() {
        normalized["text"] = json!(text);
    }
    if !fields.is_empty() {
        normalized["fields"] = Value::Object(fields);
    }
    Ok((normalized, parts.join("\n")))
}

/// 旧界面只传选项标题与 choiceId：点选时文字等于该选项标题，打字回复只算自由文字。
pub fn legacy_answer(prompt: &Value, choice_id: Option<&str>, text: &str) -> Value {
    let picked = choice_id.filter(|id| {
        prompt["options"]
            .as_array()
            .is_some_and(|o| o.iter().any(|x| x["id"] == *id && x["title"] == text))
    });
    match picked {
        Some(id) => json!({"selectedIds":[id]}),
        None => json!({"selectedIds":[],"text":text}),
    }
}

/// Composer drafts and accepted run references have different lifetimes. Old
/// sessions derive the initial draft from references not already sent by the
/// author; their accepted references remain available to the running Agent.
pub fn draft_references(session: &Value) -> Value {
    if session["draftReferences"].is_array() {return session["draftReferences"].clone();}
    let sent:BTreeSet<&str>=session["messages"].as_array().into_iter().flatten().filter(|m|m["role"]=="user")
        .flat_map(|m|m["references"].as_array().into_iter().flatten()).filter_map(|r|r["id"].as_str()).collect();
    json!(session["references"].as_array().into_iter().flatten().filter(|r|r["kind"]=="book" || r["pinned"]==true
        || !r["id"].as_str().is_some_and(|id|sent.contains(id))).cloned().collect::<Vec<_>>())
}

/// Called only after accepting a message, in the same transaction. Draft items
/// added after the send snapshot, platform books and pinned anchors survive.
pub fn consume_draft_references(session: &mut Value, sent: &Value) {
    let pending=draft_references(session);
    session["draftReferences"]=json!(pending.as_array().into_iter().flatten().filter(|r|r["kind"]=="book" || r["pinned"]==true
        || !sent.as_array().into_iter().flatten().any(|used|used["id"]==r["id"])).cloned().collect::<Vec<_>>());
}

/// A draft save can arrive after its message was accepted. Consume only IDs
/// sent since the client's last seen author message; preserve newly added files.
pub fn update_draft_references(session: &Value, incoming: &Value, base: Option<&Value>) -> Value {
    let Some(base)=base else {return incoming.clone();};
    let messages=session["messages"].as_array().cloned().unwrap_or_default();
    let from=base.as_str().and_then(|id|messages.iter().position(|m|m["id"]==id)).map_or(0,|i|i+1);
    let consumed:BTreeSet<&str>=messages[from..].iter().filter(|m|m["role"]=="user")
        .flat_map(|m|m["references"].as_array().into_iter().flatten()).filter_map(|r|r["id"].as_str()).collect();
    json!(incoming.as_array().into_iter().flatten().filter(|r|r["kind"]=="book" || r["pinned"]==true
        || !r["id"].as_str().is_some_and(|id|consumed.contains(id))).cloned().collect::<Vec<_>>())
}
