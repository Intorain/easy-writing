//! 通道档案：每个对话入口向宿主声明自己能显示什么、能发什么、默认审批策略。
//!
//! 三个入口（Agent 板块、妙笔、微信）是同一套会话的三个视图；会话记录带着它的通道档案，
//! 宿主据此组装系统提示的通道段、选择引导卡形态、决定审批与投递方式。
//! 档案只是数据：渲染规则在 worker 的系统提示里，审批在提交边界里，这里不做判断。

use serde_json::{json, Value};

pub const DESKTOP: &str = "desktop";
pub const WEIXIN: &str = "weixin";

/// 可被入口覆盖的字段；其余字段按通道类型固定
const OVERRIDABLE: [&str; 6] = ["label", "maxChars", "maxOptions", "canSendFile", "canSendImage", "approval"];

/// 某类通道的默认档案
pub fn profile(kind: &str) -> Value {
    match kind {
        WEIXIN => json!({
            "kind": WEIXIN,
            "label": "微信",
            // 微信只认纯文本：没有 Markdown、没有流式、单条有长度上限
            "plainText": true,
            "maxChars": 1800,
            "streaming": false,
            // 引导卡与确认卡渲染成编号文本，作者回编号
            "cards": "numbered",
            "maxOptions": 5,
            "canSendFile": true,
            "canSendImage": true,
            // 微信入口的写操作一律经确认卡（决策 D4）
            "approval": "always",
            "heartbeat": "alerts",
        }),
        _ => json!({
            "kind": DESKTOP,
            "label": "桌面",
            "plainText": false,
            "maxChars": 0,
            "streaming": true,
            "cards": "cards",
            "maxOptions": 8,
            "canSendFile": false,
            "canSendImage": false,
            "approval": "default",
            "heartbeat": "all",
        }),
    }
}

/// 入口建会话时给的通道：可以只给类型名，也可以给带覆盖字段的对象；未知类型拒绝。
pub fn normalize(value: &Value) -> Result<Value, String> {
    let (kind, overrides) = match value {
        Value::String(kind) => (kind.as_str(), None),
        Value::Object(map) => (map.get("kind").and_then(Value::as_str).unwrap_or(DESKTOP), Some(map)),
        Value::Null => (DESKTOP, None),
        _ => return Err("通道档案格式无效".into()),
    };
    if kind != DESKTOP && kind != WEIXIN {
        return Err(format!("未知的通道类型：{kind}"));
    }
    let mut profile = profile(kind);
    if let Some(map) = overrides {
        for key in OVERRIDABLE {
            if let Some(v) = map.get(key) {
                let same_type = matches!((&profile[key], v), (Value::String(_), Value::String(_)) | (Value::Number(_), Value::Number(_)) | (Value::Bool(_), Value::Bool(_)));
                if !same_type {
                    return Err(format!("通道档案字段 {key} 的类型不对"));
                }
                profile[key] = v.clone();
            }
        }
    }
    Ok(profile)
}

/// 会话的通道档案；旧会话没有记录时按桌面处理
pub fn of_session(session: &Value) -> Value {
    if session["channel"].is_object() {
        normalize(&session["channel"]).unwrap_or_else(|_| profile(DESKTOP))
    } else {
        profile(DESKTOP)
    }
}
