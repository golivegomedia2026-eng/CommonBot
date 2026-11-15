//! 仅用于测试和截图的假推理后端：实现 OpenAI 兼容接口里用到的两个地址。
//! 不随产品发布。

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde_json::{json, Value};
use tokio_stream::StreamExt;

#[derive(Clone, Default)]
pub struct MockConfig {
    /// 设置后要求请求带这个密钥。
    pub api_key: Option<String>,
    /// 每段之间的停顿，用来模拟模型出字的速度。
    pub delay: Duration,
}

pub fn router(cfg: MockConfig) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .with_state(cfg)
}

/// 在本机随机端口启动，返回 `http://127.0.0.1:端口/v1`。
pub async fn spawn(cfg: MockConfig) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(cfg)).await;
    });
    format!("http://{addr}/v1")
}

fn authorized(cfg: &MockConfig, headers: &HeaderMap) -> bool {
    match &cfg.api_key {
        None => true,
        Some(key) => headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {key}")),
    }
}

async fn models(State(cfg): State<MockConfig>, headers: HeaderMap) -> Response {
    if !authorized(&cfg, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "message": "bad key" } }))).into_response();
    }
    Json(json!({ "data": [{ "id": "mock-small" }, { "id": "mock-large" }] })).into_response()
}

async fn chat(State(cfg): State<MockConfig>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authorized(&cfg, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "message": "bad key" } }))).into_response();
    }
    let model = body["model"].as_str().unwrap_or("");
    if model == "missing" {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "model 'missing' not found" } })))
            .into_response();
    }
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    // 带图片的请求：当作文字识别，返回固定的「识别结果」
    let has_image = messages
        .iter()
        .any(|m| m["content"].as_array().is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url")));
    if has_image {
        let text = "<think>看图</think>评分办法\n技术分占百分之六十，商务分占百分之四十。\n投标保证金为人民币五万元。";
        return Json(json!({ "choices": [{ "message": { "role": "assistant", "content": text } }] })).into_response();
    }
    let last = messages.last().and_then(|m| m["content"].as_str()).unwrap_or("").to_string();
    let system = messages
        .first()
        .filter(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .map(|s| format!("\n- 系统提示词开头：{}", s.chars().take(24).collect::<String>()))
        .unwrap_or_default();
    // 带了知识库资料时，复述第一条资料并标注编号，模拟「依据资料回答」
    let knowledge = messages
        .first()
        .and_then(|m| m["content"].as_str())
        .and_then(|s| s.split_once("\n[1] "))
        .map(|(_, rest)| {
            let mut lines = rest.lines();
            let title = lines.next().unwrap_or("");
            let body: String = lines.next().unwrap_or("").chars().take(40).collect();
            format!("\n- 依据资料：{body} [1]（{title}）")
        })
        .or_else(|| {
            messages
                .first()
                .and_then(|m| m["content"].as_str())
                .filter(|s| s.contains("没有从知识库检索到"))
                .map(|_| "\n- 知识库里没有找到相关内容".to_string())
        })
        .unwrap_or_default();
    let reply = format!(
        "这是测试用的假模型（{model}）。\n\n你说的是：**{last}**\n\n- 这次请求带了 {} 条消息\n- 回答是一段一段流式返回的{system}{knowledge}",
        messages.len()
    );
    // 按字符切成小段，故意让汉字落在不同的数据块里。
    let chars: Vec<char> = reply.chars().collect();
    let mut events: Vec<Value> = vec![json!({ "choices": [{ "delta": { "reasoning_content": "先想一想。" } }] })];
    events.extend(
        chars.chunks(3).map(|c| json!({ "choices": [{ "delta": { "content": c.iter().collect::<String>() } }] })),
    );
    Sse::new(sse(events, cfg.delay)).into_response()
}

fn sse(events: Vec<Value>, delay: Duration) -> impl Stream<Item = Result<Event, Infallible>> {
    let total = events.len();
    tokio_stream::iter(
        events.into_iter().map(|e| e.to_string()).chain(std::iter::once("[DONE]".to_string())).enumerate(),
    )
    .then(move |(i, data)| async move {
        if !delay.is_zero() && i > 0 && i <= total {
            tokio::time::sleep(delay).await;
        }
        Ok(Event::default().data(data))
    })
}

/// 假的向量化：按相邻两个字符哈希到 64 维。内容相近的文字得到相近的向量，足够测试检索流程。
async fn embeddings(State(cfg): State<MockConfig>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authorized(&cfg, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "message": "bad key" } }))).into_response();
    }
    if body["model"] == "missing" {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "model 'missing' not found" } })))
            .into_response();
    }
    let inputs: Vec<String> = match &body["input"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => vec![],
    };
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let mut v = vec![0f32; 64];
            let chars: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
            for w in chars.windows(2) {
                let h = (w[0] as u32).wrapping_mul(31).wrapping_add(w[1] as u32).wrapping_mul(2654435761);
                v[(h % 64) as usize] += 1.0;
            }
            json!({ "index": i, "embedding": v })
        })
        .collect();
    Json(json!({ "data": data })).into_response()
}
