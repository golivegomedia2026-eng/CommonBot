//! 发布出去的智能体：独立链接（访客在网页上对话）和兼容 OpenAI 的接口（供其他系统调用）。
//! 这两条路不经过主人的访问码，各自凭链接里的令牌或 API 密钥进入，只能用到那一个智能体。

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonbot_core::agent::{self, Agent};
use commonbot_core::llm::ChatMessage;
use commonbot_core::store::{self, Provider};
use commonbot_core::{new_id, now_ms, Error};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::chat::{run_turn, send_in_conversation, SendOptions, Turn, TurnEvent};
use crate::{ApiResult, AppState};

// ---------- 独立链接 ----------

pub fn public() -> Router<AppState> {
    Router::new()
        .route("/{token}", get(info))
        .route("/{token}/conversations", post(new_conversation))
        .route("/{token}/conversations/{id}", get(read_conversation))
        .route("/{token}/conversations/{id}/messages", post(say))
}

async fn agent_for(st: &AppState, token: String) -> ApiResult<Agent> {
    Ok(st.db.run(move |c| agent::by_token(c, &token)).await?)
}

/// 访客能看到的只有这些：名称、简介、开场白、示例问题。
async fn info(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    let a = agent_for(&st, token).await?;
    Ok(Json(json!({ "name": a.name, "description": a.description, "opening": a.opening, "examples": a.examples })))
}

async fn new_conversation(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    let a = agent_for(&st, token).await?;
    let conv = st.db.run(move |c| store::create_conversation_from(c, &a.id, "link")).await?;
    Ok(Json(json!({ "id": conv.id })))
}

/// 确认这个对话确实是通过这个链接建的：访客拿不到主人的对话，也拿不到别的智能体的对话。
async fn visitor_conversation(st: &AppState, token: String, id: String) -> ApiResult<()> {
    let a = agent_for(st, token).await?;
    let conv = st.db.run(move |c| store::get_conversation(c, &id)).await?;
    if conv.agent_id != a.id || conv.origin != "link" {
        return Err(Error::NotFound("conversation").into());
    }
    Ok(())
}

async fn read_conversation(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    visitor_conversation(&st, token, id.clone()).await?;
    let messages = st.db.run(move |c| store::list_messages(c, &id)).await?;
    Ok(Json(json!({ "messages": messages })))
}

#[derive(Deserialize)]
struct Say {
    content: String,
}

async fn say(
    State(st): State<AppState>,
    Path((token, id)): Path<(String, String)>,
    Json(body): Json<Say>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    visitor_conversation(&st, token, id.clone()).await?;
    send_in_conversation(st, id, &body.content, SendOptions::default()).await
}

// ---------- 兼容 OpenAI 的接口 ----------

pub fn openai() -> Router<AppState> {
    Router::new().route("/chat/completions", post(completions)).route("/models", get(models))
}

/// 按 OpenAI 的格式返回错误，调用方的现成客户端才能看懂。
fn api_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": { "message": message.into(), "type": "invalid_request_error", "code": code } })))
        .into_response()
}

async fn agent_for_key(st: &AppState, headers: &HeaderMap) -> Result<(Agent, Provider, String), Response> {
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .trim()
        .to_string();
    let unauthorized = || api_error(StatusCode::UNAUTHORIZED, "invalid_api_key", "Incorrect API key provided.");
    if key.is_empty() {
        return Err(unauthorized());
    }
    let found = st
        .db
        .run(move |c| {
            let a = agent::by_api_key(c, &key)?;
            let provider = match &a.provider_id {
                Some(pid) => store::get_provider(c, pid).ok(),
                None => None,
            };
            Ok((a, provider))
        })
        .await;
    match found {
        Ok((a, Some(provider))) => match a.model.clone() {
            Some(model) => Ok((a, provider, model)),
            None => {
                Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "model_not_set", "This agent has no model configured."))
            }
        },
        Ok((_, None)) => Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "model_not_set",
            "This agent's model service is no longer available.",
        )),
        Err(_) => Err(unauthorized()),
    }
}

async fn models(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match agent_for_key(&st, &headers).await {
        Ok((a, _, _)) => {
            Json(json!({ "object": "list", "data": [{ "id": a.name, "object": "model", "owned_by": "commonbot" }] }))
                .into_response()
        }
        Err(e) => e,
    }
}

/// 消息内容可以是字符串，也可以是分段的数组；只取其中的文字。
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
struct CompletionBody {
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    stream: bool,
}

async fn completions(State(st): State<AppState>, headers: HeaderMap, Json(body): Json<CompletionBody>) -> Response {
    let (agent, provider, model) = match agent_for_key(&st, &headers).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    // 调用方自带的系统提示词不采用：这个接口代表的是这个智能体，它的定义由主人决定
    let history: Vec<ChatMessage> = body
        .messages
        .iter()
        .filter_map(|m| {
            let role = m["role"].as_str()?;
            let content = text_of(&m["content"]);
            (matches!(role, "user" | "assistant") && !content.trim().is_empty())
                .then(|| ChatMessage { role: role.into(), content })
        })
        .collect();
    let Some(question) = history.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_messages",
            "messages must include at least one user message.",
        );
    };
    let previous = history.iter().rev().filter(|m| m.role == "user").nth(1).map(|m| m.content.as_str()).unwrap_or("");
    let name = agent.name.clone();
    let turn = Turn {
        query: format!("{previous}\n{question}"),
        question,
        history,
        files: Vec::new(),
        folders: agent.folders.clone(),
        web_mode: agent.web.clone(),
        agent,
        provider,
        model,
    };
    let (id, created) = (format!("chatcmpl-{}", new_id()), now_ms() / 1000);
    let (ttx, mut trx) = mpsc::channel::<TurnEvent>(64);
    let worker = {
        let st = st.clone();
        tokio::spawn(async move { run_turn(&st, turn, ttx).await })
    };

    if !body.stream {
        while trx.recv().await.is_some() {}
        let Ok(out) = worker.await else {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal", "The request could not be completed.");
        };
        if let Some(e) = out.failure {
            return api_error(StatusCode::BAD_GATEWAY, e.code(), e.to_string());
        }
        return Json(json!({
            "id": id, "object": "chat.completion", "created": created, "model": name,
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": out.text }, "finish_reason": "stop" }],
            "sources": out.sources,
        }))
        .into_response();
    }

    let (tx, rx) = mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        let chunk = |delta: Value, finish: Value| {
            Event::default().data(
                json!({ "id": id, "object": "chat.completion.chunk", "created": created, "model": name,
                        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
                .to_string(),
            )
        };
        let _ = tx.send(chunk(json!({ "role": "assistant" }), Value::Null)).await;
        while let Some(ev) = trx.recv().await {
            if let TurnEvent::Delta(t) = ev {
                if tx.send(chunk(json!({ "content": t }), Value::Null)).await.is_err() {
                    break;
                }
            }
        }
        drop(trx);
        let last = match worker.await {
            Ok(out) => match out.failure {
                None => {
                    let mut end = json!({ "id": id, "object": "chat.completion.chunk", "created": created, "model": name,
                        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] });
                    end["sources"] = out.sources;
                    Event::default().data(end.to_string())
                }
                Some(e) => Event::default()
                    .data(json!({ "error": { "message": e.to_string(), "code": e.code() } }).to_string()),
            },
            Err(_) => Event::default()
                .data(json!({ "error": { "message": "internal error", "code": "internal" } }).to_string()),
        };
        let _ = tx.send(last).await;
        let _ = tx.send(Event::default().data("[DONE]")).await;
    });
    Sse::new(ReceiverStream::new(rx).map(Ok::<_, Infallible>)).keep_alive(KeepAlive::default()).into_response()
}
