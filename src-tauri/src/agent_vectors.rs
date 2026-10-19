//! 向量检索层（全平台方案状态层的第四层）：章节切块与记忆各存一份向量，按"意思"而不只按字面找前文与记忆。
//!
//! 向量由作者在模型管理里配置的向量模型（分组 embedding，OpenAI 兼容的 POST {base}/embeddings）生成；
//! 没有配置就什么都不做，检索只走全文索引，界面与模型工具的行为不变。
//! 向量存在 SQLite 里（f32 小端字节，按切块编号），查询时在 Rust 里逐条算余弦相似度：一本书几百到几千块，毫秒级，
//! 不引入向量扩展。索引器每轮结束后补齐缺的向量；切块重建后旧向量随之清掉；换了向量模型就全部重算。
use crate::agent_store::*;
use crate::model_gateway_providers::{self as providers, ModelProfile};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};
use std::future::Future;
use std::pin::Pin;

pub const VECTOR_DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS chapter_vectors(chunkId INTEGER PRIMARY KEY,model TEXT NOT NULL,dim INTEGER NOT NULL,vector BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS memory_vectors(chunkId INTEGER PRIMARY KEY,model TEXT NOT NULL,dim INTEGER NOT NULL,vector BLOB NOT NULL)",
];
/// 一次请求送多少段
const BATCH: usize = 32;
/// 一轮最多补多少块：索引器每轮结束顺手补一点，不让一本大书的首次补齐拖住别的事
pub const MAX_PER_PASS: usize = 400;
/// 倒数排名融合的平滑常数（RRF 的经典取值）
const RRF_K: f64 = 60.0;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub type EmbedFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, String>> + Send + 'a>>;

/// 把一批文本变成一批向量（顺序与输入一致）。真实实现走供应商接口；测试用确定性的假实现。
pub trait Embedder: Send + Sync {
    fn embed<'a>(&'a self, texts: Vec<String>) -> EmbedFuture<'a>;
}

/// 作者配置的向量模型：分组偏好里的那一个，没设就取第一个启用的向量类模型；没有就 None
pub async fn embedding_model(db: &mut SqliteConnection) -> Result<Option<Value>, String> {
    let Some(raw) = get(db, "app-settings", "ew-local-ai-models").await? else { return Ok(None) };
    let models: Value = match raw {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        other => other,
    };
    let usable = |m: &&Value| m["status"] == 1 && m["scene"] == "embedding";
    let list = models["models"].as_array().cloned().unwrap_or_default();
    let preferred = models["preferences"]["embedding"].as_str().filter(|v| !v.is_empty());
    let picked = preferred
        .and_then(|code| list.iter().filter(usable).find(|m| m["id"].to_string() == code || m["id"].as_str() == Some(code)))
        .or_else(|| list.iter().find(usable));
    Ok(picked.cloned())
}

/// 向量的"身份"：模型记录编号加模型名；换了就全部重算
pub fn model_key(model: &Value) -> String {
    format!("{}:{}", model["id"], model["modelCode"].as_str().unwrap_or(""))
}

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().expect("http client"))
}

/// 供应商接口：POST {base}/embeddings，OpenAI 兼容形状
pub struct RemoteEmbedder {
    profile: ModelProfile,
    root: Option<std::path::PathBuf>,
}

impl RemoteEmbedder {
    pub fn new(model: &Value, root: Option<&std::path::Path>) -> Self {
        Self { profile: ModelProfile::from_record(model), root: root.map(|r| r.to_path_buf()) }
    }
}

impl Embedder for RemoteEmbedder {
    fn embed<'a>(&'a self, texts: Vec<String>) -> EmbedFuture<'a> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let url = providers::join_url(&self.profile.base_url, "embeddings");
            let body = json!({"model":self.profile.model_code,"input":texts});
            let mut request = client().post(&url).header(reqwest::header::CONTENT_TYPE, "application/json").json(&body);
            if !self.profile.api_key.is_empty() {
                request = request.bearer_auth(&self.profile.api_key);
            }
            let outcome: Result<(Vec<Vec<f32>>, Option<u64>), String> = async {
                let response = request.send().await.map_err(|e| format!("向量接口连接失败：{e}"))?;
                let status = response.status();
                let text = response.text().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(providers::translate_status(status.as_u16(), &providers::error_detail(&text)));
                }
                let parsed: Value = serde_json::from_str(&text).map_err(|_| "向量接口返回的不是 JSON".to_string())?;
                let mut items: Vec<(usize, Vec<f32>)> = parsed["data"]
                    .as_array()
                    .ok_or("向量接口没有返回 data")?
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let index = item["index"].as_u64().map(|v| v as usize).unwrap_or(i);
                        let vector = item["embedding"].as_array().into_iter().flatten().filter_map(Value::as_f64).map(|v| v as f32).collect::<Vec<_>>();
                        (index, vector)
                    })
                    .collect();
                items.sort_by_key(|(i, _)| *i);
                if items.len() != texts.len() || items.iter().any(|(_, v)| v.is_empty()) {
                    return Err("向量接口返回的数量或维度不对，请确认所选模型是 embedding（向量）类型".into());
                }
                Ok((items.into_iter().map(|(_, v)| v).collect(), parsed["usage"]["total_tokens"].as_u64()))
            }
            .await;
            if let Some(root) = &self.root {
                let record = crate::ai_trace::AiCallRecord {
                    scene: "embedding".into(),
                    scene_label: "向量检索".into(),
                    model_code: self.profile.id.clone(),
                    model_name: self.profile.name.clone(),
                    ok: outcome.is_ok(),
                    input: format!("{} 段文本", texts.len()),
                    output: outcome.as_ref().map(|(v, _)| format!("{} 个向量，{} 维", v.len(), v.first().map_or(0, Vec::len))).unwrap_or_default(),
                    input_tokens: outcome.as_ref().ok().and_then(|(_, usage)| *usage),
                    output_tokens: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    error: outcome.as_ref().err().cloned(),
                };
                let _ = crate::ai_trace::record(root, record).await;
            }
            outcome.map(|(vectors, _)| vectors)
        })
    }
}

// ---------------------------------------------------------------------------
// 存取与相似度
// ---------------------------------------------------------------------------

pub fn to_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn from_blob(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

/// 补齐缺的向量：先清掉已不存在的切块和别的模型算的向量，再给没有向量的切块与记忆算；一轮最多 `limit` 块。
/// 返回这一轮写了多少。供应商出错时返回错误，已写入的保留。
pub async fn fill_missing(db: &mut SqliteConnection, model_key: &str, embedder: &dyn Embedder, limit: usize) -> Result<usize, String> {
    for (vectors, chunks) in [("chapter_vectors", "chapter_chunks"), ("memory_vectors", "memory_chunks")] {
        sqlx::query(&format!("DELETE FROM {vectors} WHERE model<>? OR chunkId NOT IN (SELECT id FROM {chunks})"))
            .bind(model_key)
            .execute(&mut *db)
            .await
            .map_err(err)?;
    }
    let mut written = 0usize;
    for (vectors, chunks) in [("chapter_vectors", "chapter_chunks"), ("memory_vectors", "memory_chunks")] {
        if written >= limit {
            break;
        }
        let rows = sqlx::query(&format!("SELECT c.id AS id,c.text AS text FROM {chunks} c LEFT JOIN {vectors} v ON v.chunkId=c.id WHERE v.chunkId IS NULL ORDER BY c.id LIMIT ?"))
            .bind((limit - written) as i64)
            .fetch_all(&mut *db)
            .await
            .map_err(err)?;
        let pending: Vec<(i64, String)> = rows.iter().map(|r| (r.get::<i64, _>("id"), r.get::<String, _>("text"))).collect();
        for batch in pending.chunks(BATCH) {
            let texts = batch.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>();
            let embedded = embedder.embed(texts).await?;
            for ((id, _), vector) in batch.iter().zip(embedded) {
                sqlx::query(&format!("INSERT OR REPLACE INTO {vectors}(chunkId,model,dim,vector) VALUES(?,?,?,?)"))
                    .bind(id)
                    .bind(model_key)
                    .bind(vector.len() as i64)
                    .bind(to_blob(&vector))
                    .execute(&mut *db)
                    .await
                    .map_err(err)?;
                written += 1;
            }
        }
    }
    Ok(written)
}

/// 还有多少切块与记忆没有向量
pub async fn missing_count(db: &mut SqliteConnection, model_key: &str) -> Result<i64, String> {
    let mut total = 0i64;
    for (vectors, chunks) in [("chapter_vectors", "chapter_chunks"), ("memory_vectors", "memory_chunks")] {
        total += sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {chunks} c LEFT JOIN {vectors} v ON v.chunkId=c.id AND v.model=? WHERE v.chunkId IS NULL"))
            .bind(model_key)
            .fetch_one(&mut *db)
            .await
            .map_err(err)?;
    }
    Ok(total)
}

/// 这本书的切块按意思排序：(切块编号, 相似度)，高的在前；没有向量时空
pub async fn similar_chunks(db: &mut SqliteConnection, book_id: &str, query: &[f32], limit: usize) -> Result<Vec<(i64, f32)>, String> {
    let rows = sqlx::query("SELECT v.chunkId AS id,v.vector AS vector FROM chapter_vectors v JOIN chapter_chunks c ON c.id=v.chunkId WHERE c.bookId=?")
        .bind(book_id)
        .fetch_all(&mut *db)
        .await
        .map_err(err)?;
    Ok(rank(rows.iter().map(|r| (r.get::<i64, _>("id"), from_blob(r.get::<Vec<u8>, _>("vector").as_slice()))), query, limit))
}

/// 这本书加作者层的记忆按意思排序
pub async fn similar_memories(db: &mut SqliteConnection, book_id: &str, query: &[f32], limit: usize) -> Result<Vec<(i64, f32)>, String> {
    let rows = sqlx::query("SELECT v.chunkId AS id,v.vector AS vector FROM memory_vectors v JOIN memory_chunks m ON m.id=v.chunkId WHERE m.bookId=? OR m.scope='author'")
        .bind(book_id)
        .fetch_all(&mut *db)
        .await
        .map_err(err)?;
    Ok(rank(rows.iter().map(|r| (r.get::<i64, _>("id"), from_blob(r.get::<Vec<u8>, _>("vector").as_slice()))), query, limit))
}

fn rank(items: impl Iterator<Item = (i64, Vec<f32>)>, query: &[f32], limit: usize) -> Vec<(i64, f32)> {
    let mut scored: Vec<(i64, f32)> = items.map(|(id, v)| (id, cosine(&v, query))).filter(|(_, s)| *s > 0.0).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

/// 倒数排名融合：两份排序各按名次给分（1/(k+名次)），同一块累加；两边都有的自然排前面。
/// 返回 (切块编号, 融合分, 来源)，来源 fts / semantic / both。
pub fn fuse(fts: &[i64], semantic: &[i64], limit: usize) -> Vec<(i64, f64, &'static str)> {
    let mut scores: std::collections::BTreeMap<i64, (f64, bool, bool)> = std::collections::BTreeMap::new();
    for (rank, id) in fts.iter().enumerate() {
        let entry = scores.entry(*id).or_insert((0.0, false, false));
        entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        entry.1 = true;
    }
    for (rank, id) in semantic.iter().enumerate() {
        let entry = scores.entry(*id).or_insert((0.0, false, false));
        entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        entry.2 = true;
    }
    let mut out: Vec<(i64, f64, &'static str)> = scores
        .into_iter()
        .map(|(id, (score, f, s))| (id, score, if f && s { "both" } else if s { "semantic" } else { "fts" }))
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}
