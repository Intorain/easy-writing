//! 轨迹账本：模型调用在宿主只记一处。
//!
//! 本期先把网关的每次调用写进前端已有的"AI 调用记录"账本（同一命名空间、同一记录形状），
//! 记录页不改就能看到 Agent 的调用；运行、工具调用、审批、投递的记录后续步骤并入同一账本。

use serde_json::{json, Value};
use sqlx::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub const RECORDS_NS: &str = "ew-local-ai-records/records";
/// 与前端一致：只留最近 1000 条，正文各留 2000 字
pub const MAX_RECORDS: usize = 1000;
const TEXT_KEEP_CHARS: usize = 2000;

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct AiCallRecord {
    pub scene: String,
    pub scene_label: String,
    /// 模型管理里的记录编号（字符串形式），与前端 `localAiModelCode` 口径一致
    pub model_code: String,
    pub model_name: String,
    pub ok: bool,
    pub input: String,
    pub output: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// 拿不到真实 usage 时的估算口径：中文 ≈ 1.7 字/token，与前端相同
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as f64 / 1.7).round() as u64
}

fn keep(text: &str) -> String {
    text.chars().take(TEXT_KEEP_CHARS).collect()
}

/// 与前端 `createLocalEntityId` 同一取值空间：负的毫秒时间戳 ×1000 加序号，越新越小
fn next_id() -> i64 {
    let now = chrono::Utc::now().timestamp_millis() as u64;
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) % 1000;
    -((now * 1000 + seq) as i64)
}

pub fn to_value(record: &AiCallRecord) -> Value {
    let mut value = json!({
        "id": next_id(),
        "recordType": "text",
        "scene": record.scene,
        "sceneLabel": record.scene_label,
        "modelCode": record.model_code,
        "modelName": record.model_name,
        "status": if record.ok { 1 } else { 0 },
        "input": keep(&record.input),
        "output": keep(&record.output),
        "inputTokens": record.input_tokens.unwrap_or_else(|| estimate_tokens(&record.input)),
        "outputTokens": record.output_tokens.unwrap_or_else(|| estimate_tokens(&record.output)),
        "duration": record.duration_ms,
        "createTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    });
    if let Some(error) = &record.error {
        value["errorMsg"] = Value::String(error.clone());
    }
    value
}

/// 落账永不影响调用本身：调用方只把错误写进日志。
pub async fn record(root: &Path, record: AiCallRecord) -> Result<(), String> {
    let value = to_value(&record);
    let key = value["id"].clone();
    let mut db = crate::desktop_storage::connect_at(root).await?;
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM desktop_records WHERE namespace=? ORDER BY CAST(key AS INTEGER) DESC",
    )
    .bind(RECORDS_NS)
    .fetch_all(&mut db)
    .await
    .map_err(|e| e.to_string())?;
    db.close().await.map_err(|e| e.to_string())?;
    // 账本按 id 倒序即按时间正序：最旧的在最前，超量时砍掉最前面的
    let excess = (keys.len() + 1).saturating_sub(MAX_RECORDS);
    let remove: Vec<Value> = keys
        .iter()
        .take(excess)
        .filter_map(|k| serde_json::from_str::<Value>(k).ok())
        .collect();
    crate::desktop_storage::write_at(
        root,
        RECORDS_NS,
        vec![crate::desktop_storage::Record { key, value: value.to_string() }],
        remove,
        false,
        false,
    )
    .await
}
