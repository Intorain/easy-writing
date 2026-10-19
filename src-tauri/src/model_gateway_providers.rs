//! 供应商差异表：全平台只有这一份。
//!
//! 内容与 `src/utils/local-ai-client.ts` 里直接场景用的那张表一致，直接场景改走网关后那份即可删除。
//! 这里只处理"同一个 OpenAI 兼容请求，各家接受的字段差在哪"，不改提示词、不做业务判断。

use serde_json::{json, Map, Value};

/// 非流式请求最少给足的输出上限：思考型模型把思考计入输出上限，太小会拿回空正文或半截 JSON。
pub const NON_STREAM_MIN_TOKENS: u64 = 2048;

/// 模型管理里的一条模型记录中，网关需要的部分。
#[derive(Debug, Clone)]
pub struct ModelProfile {
    pub id: String,
    pub name: String,
    pub model_code: String,
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    /// "on" / "off" / 其他 = 跟随模型默认，不下发任何字段
    pub thinking: String,
    /// 作者在模型管理里填的"额外请求参数"（JSON 对象文本）
    pub extra_params: Option<Map<String, Value>>,
    /// 作者按角色另配的采样参数：`{"agent_writer":{"temperature":1.3},"agent_critic":{...}}`；没配的角色用默认表
    pub sampling: Option<Map<String, Value>>,
}

impl ModelProfile {
    pub fn from_record(model: &Value) -> Self {
        let text = |key: &str| model[key].as_str().unwrap_or("").trim().to_owned();
        let extra_params = model["extraParams"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v.as_object().cloned());
        let base_url = text("baseUrl");
        let sampling = match &model["sampling"] {
            Value::Object(map) => Some(map.clone()),
            Value::String(s) => serde_json::from_str::<Value>(s).ok().and_then(|v| v.as_object().cloned()),
            _ => None,
        };
        Self {
            id: match &model["id"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
            name: text("name"),
            model_code: text("modelCode"),
            provider: infer_provider(&text("provider"), &base_url),
            base_url,
            api_key: text("apiKey"),
            thinking: text("thinking"),
            extra_params,
            sampling,
        }
    }
}

/// 按角色的默认采样参数。评审与索引要稳；这是调用参数，不是写法规则，写作模型看不到它。
/// 写作角色默认不下发温度、用供应商默认值：DeepSeek 文档里"创意写作 1.5"是老模型的经验，
/// 2026-10-17 用 deepseek-v4-flash 回放 20 个场景，1.5 时正文写到几百字就开始碎句乱码，读者盲比全部落败。
/// 调用方显式传了、作者在模型记录的 `sampling` 字段里按角色配了，都优先于这张表。
pub fn sampling_defaults(scene: &str, _provider: &str) -> Option<Value> {
    match scene {
        "agent_critic" | "agent_indexer" => Some(json!({"temperature": 0.2})),
        _ => None,
    }
}

const SAMPLING_KEYS: [&str; 2] = ["temperature", "top_p"];

fn apply_sampling(body: &mut Map<String, Value>, profile: &ModelProfile, scene: &str) {
    let configured = profile
        .sampling
        .as_ref()
        .and_then(|s| s.get(scene))
        .and_then(Value::as_object)
        .cloned();
    let defaults = sampling_defaults(scene, &profile.provider).and_then(|v| v.as_object().cloned());
    let Some(sampling) = configured.or(defaults) else {
        return;
    };
    for key in SAMPLING_KEYS {
        if let Some(value) = sampling.get(key).filter(|v| v.is_number()) {
            body.entry(key.to_owned()).or_insert_with(|| value.clone());
        }
    }
}

/// 供应商代码来自模型管理的预设；自定义或未知时按地址猜官方渠道，老配置也能命中翻译表。
pub fn infer_provider(provider: &str, base_url: &str) -> String {
    let code = provider.trim();
    if !code.is_empty() && code != "custom" {
        return code.to_owned();
    }
    let url = base_url;
    let guess = if url.contains("api.deepseek.com") {
        "deepseek"
    } else if url.contains("aliyuncs.com") {
        "aliyun"
    } else if url.contains("siliconflow") {
        "siliconflow"
    } else if url.contains("bigmodel.cn") {
        "bigmodel"
    } else if url.contains("volces.com") {
        "volcengine"
    } else if url.contains("openrouter.ai") {
        "openrouter"
    } else if url.contains("googleapis.com") {
        "gemini_openai"
    } else {
        code
    };
    guess.to_owned()
}

/// 思考开关翻译：各家字段不同；表里没有的供应商不下发任何字段，由作者在额外参数里自填。
pub fn thinking_params(provider: &str, on: bool) -> Option<Value> {
    match provider {
        "deepseek" | "bigmodel" | "volcengine" => {
            Some(json!({"thinking": {"type": if on { "enabled" } else { "disabled" }}}))
        }
        "aliyun" | "siliconflow" => Some(json!({"enable_thinking": on})),
        "openrouter" => Some(json!({"reasoning": {"enabled": on}})),
        "gemini_openai" | "local" => (!on).then(|| json!({"reasoning_effort": "none"})),
        _ => None,
    }
}

/// DeepSeek 开启思考时只认 auto / none，传 required 服务端直接 400；按供应商降级，不白打一次失败请求。
pub fn forced_tool_choice_unsupported(provider: &str, thinking_on: bool) -> bool {
    matches!(provider, "deepseek") && thinking_on
}

/// Data extraction is not an Agent tool invocation. Select the provider's
/// documented document transport here; domain workers never branch on a model
/// name. JSON mode still requires local schema and evidence validation.
pub fn structured_output_mode(provider: &str) -> &'static str {
    match provider {
        "deepseek" => "json_object",
        _ => "tool",
    }
}

pub fn is_openai_official(base_url: &str) -> bool {
    base_url.contains("api.openai.com")
}

/// OpenAI 官方推理系（o*/gpt-5*）只认默认温度
pub fn is_openai_reasoning_model(model_code: &str) -> bool {
    let code = model_code.trim().to_ascii_lowercase();
    code.starts_with('o') && code[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        || code.starts_with("gpt-5")
}

/// baseUrl 与端点拼接：只负责去重斜杠，版本段（/v1 等）以作者填写为准
pub fn join_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

const OUTPUT_CAP_KEYS: [&str; 3] = ["max_tokens", "max_completion_tokens", "max_output_tokens"];

/// 把调用方（worker）发来的标准 OpenAI 请求改写成该供应商实际接受的形状。
/// 调用方的输出上限是上下文预算的一部分，额外参数里的输出上限不能绕过它。
pub fn adapt_request(body: &mut Map<String, Value>, profile: &ModelProfile) {
    adapt_request_for(body, profile, "");
}

/// 带场景码的改写：场景码决定按哪个角色补默认采样参数（见 `sampling_defaults`）。
pub fn adapt_request_for(body: &mut Map<String, Value>, profile: &ModelProfile, scene: &str) {
    body.insert("model".into(), Value::String(profile.model_code.clone()));
    apply_sampling(body, profile, scene);
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let thinking_on = profile.thinking == "on";

    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty())
    {
        // "必须调用工具"有两种写法：字符串 required，或指定某个函数的对象（结构化输出就是这种）；
        // 不支持的供应商上都降成 auto，不白打一次 400。
        let forced = match body.get("tool_choice") {
            Some(Value::String(choice)) => choice == "required",
            Some(Value::Object(_)) => true,
            _ => false,
        };
        if forced && forced_tool_choice_unsupported(&profile.provider, thinking_on) {
            body.insert("tool_choice".into(), Value::String("auto".into()));
        }
    } else {
        body.remove("tool_choice");
    }

    let requested = OUTPUT_CAP_KEYS
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_u64));
    for key in OUTPUT_CAP_KEYS {
        body.remove(key);
    }
    if let Some(mut limit) = requested {
        if !stream {
            limit = limit.max(NON_STREAM_MIN_TOKENS);
        }
        let field = if is_openai_official(&profile.base_url) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(field.into(), Value::from(limit));
    }

    if is_openai_official(&profile.base_url) && is_openai_reasoning_model(&profile.model_code) {
        body.remove("temperature");
    }

    if profile.thinking == "on" || profile.thinking == "off" {
        if let Some(Value::Object(extra)) = thinking_params(&profile.provider, thinking_on) {
            for (key, value) in extra {
                body.insert(key, value);
            }
        }
    }
    // 千问百炼：思考型模型走非流式必须 enable_thinking=false，否则服务端直接 400。
    if !stream && profile.provider == "aliyun" {
        body.insert("enable_thinking".into(), Value::Bool(false));
    }

    if let Some(extra) = &profile.extra_params {
        for (key, value) in extra {
            if OUTPUT_CAP_KEYS.contains(&key.as_str()) {
                continue;
            }
            body.insert(key.clone(), value.clone());
        }
    }
}

/// 把供应商的失败翻译成作者能懂的中文；原始状态码保留在括号里供排障。
pub fn translate_status(status: u16, detail: &str) -> String {
    let base = match status {
        401 => "API Key 无效或未授权".to_owned(),
        402 => "账户余额不足，请到供应商后台充值".to_owned(),
        403 => "没有访问权限：云端服务请检查 Key 的可用范围，本地服务请检查它的来源或 IP 访问限制"
            .to_owned(),
        404 => "接口路径或模型不存在（检查 BaseURL 与模型名）".to_owned(),
        429 => "触发限流或额度不足".to_owned(),
        _ => format!("请求失败（HTTP {status}）"),
    };
    let detail: String = detail.trim().chars().take(200).collect();
    if detail.is_empty() {
        base
    } else {
        format!("{base}：{detail}")
    }
}

/// 从供应商的错误响应体里取可读说明：`error.message` 或 `message`，否则截取原文。
pub fn error_detail(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(provider: &str, thinking: &str) -> ModelProfile {
        ModelProfile::from_record(
            &json!({"id":1,"name":"m","modelCode":"m","provider":provider,
            "baseUrl":"https://api.deepseek.com","apiKey":"k","thinking":thinking}),
        )
    }

    fn body(tool_choice: Value) -> Map<String, Value> {
        json!({"messages":[],"tools":[{"type":"function","function":{"name":"t","parameters":{"type":"object"}}}],
            "tool_choice":tool_choice}).as_object().cloned().unwrap()
    }

    #[test]
    fn deepseek_thinking_downgrades_every_forced_tool_choice_to_auto() {
        let forced = json!({"type":"function","function":{"name":"extract"}});
        let mut b = body(forced.clone());
        adapt_request(&mut b, &profile("deepseek", "on"));
        assert_eq!(b["tool_choice"], "auto", "指定函数名的写法也降级");
        let mut b = body(json!("required"));
        adapt_request(&mut b, &profile("deepseek", "on"));
        assert_eq!(b["tool_choice"], "auto");
        let mut b = body(json!("none"));
        adapt_request(&mut b, &profile("deepseek", "on"));
        assert_eq!(b["tool_choice"], "none", "none 本来就支持");
        let mut b = body(forced.clone());
        adapt_request(&mut b, &profile("deepseek", "off"));
        assert_eq!(
            b["tool_choice"]["function"]["name"], "extract",
            "不思考时原样下发"
        );
        let mut b = body(forced);
        adapt_request(&mut b, &profile("openai", "on"));
        assert_eq!(
            b["tool_choice"]["function"]["name"], "extract",
            "其他供应商不动"
        );
    }

    #[test]
    fn sampling_follows_the_role_unless_caller_or_author_decides() {
        let plain = || json!({"messages":[],"stream":true}).as_object().cloned().unwrap();
        let mut b = plain();
        adapt_request_for(&mut b, &profile("deepseek", "off"), "agent_writer");
        assert!(b.get("temperature").is_none(), "写作角色默认用供应商温度：V4 Flash 在 1.5 会乱码");
        let mut b = plain();
        adapt_request_for(&mut b, &profile("openai", "off"), "agent_writer");
        assert!(b.get("temperature").is_none(), "没有供应商建议的写作角色不下发");
        let mut b = plain();
        adapt_request_for(&mut b, &profile("deepseek", "off"), "agent_critic");
        assert_eq!(b["temperature"], 0.2);
        let mut b = plain();
        adapt_request_for(&mut b, &profile("deepseek", "off"), "agent_planner");
        assert!(b.get("temperature").is_none(), "规划角色用供应商默认");
        let mut b = plain();
        b.insert("temperature".into(), json!(0.9));
        adapt_request_for(&mut b, &profile("deepseek", "off"), "agent_writer");
        assert_eq!(b["temperature"], 0.9, "调用方显式传的优先");
        let mut record = json!({"id":1,"name":"m","modelCode":"m","provider":"deepseek","baseUrl":"https://api.deepseek.com","apiKey":"k",
            "sampling":{"agent_writer":{"temperature":1.1,"top_p":0.95}}});
        let mut b = plain();
        adapt_request_for(&mut b, &ModelProfile::from_record(&record), "agent_writer");
        assert_eq!(b["temperature"], 1.1);
        assert_eq!(b["top_p"], 0.95);
        record["extraParams"] = json!("{\"temperature\":0.5}");
        let mut b = plain();
        adapt_request_for(&mut b, &ModelProfile::from_record(&record), "agent_writer");
        assert_eq!(b["temperature"], 0.5, "作者的额外参数覆盖一切");
        adapt_request(&mut plain(), &profile("deepseek", "off"));
    }
}
