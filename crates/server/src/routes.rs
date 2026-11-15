use std::convert::Infallible;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use commonbot_core::llm::{ChatMessage, Delta, Endpoint};
use commonbot_core::store::{self, Conversation, Message, Provider, ProviderInput};
use commonbot_core::Error;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::access::{self, Caller};
use crate::{ApiError, ApiResult, AppState, VERSION};

/// 一条消息的长度上限（字符）。
const MAX_MESSAGE_CHARS: usize = 200_000;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/access", post(enter_access_code))
        .route("/access-code", get(show_access_code).post(reset_access_code))
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/{id}", axum::routing::put(update_provider).delete(delete_provider))
        .route("/providers/{id}/check", post(check_provider))
        .route("/conversations", get(list_conversations).post(create_conversation))
        .route("/conversations/{id}", get(get_conversation).patch(rename_conversation).delete(delete_conversation))
        .route("/conversations/{id}/messages", post(send_message))
}

// ---------- 基本信息与访问码 ----------

async fn health(State(st): State<AppState>, Extension(caller): Extension<Caller>) -> Json<Value> {
    Json(json!({
        "name": "CommonBot",
        "version": VERSION,
        "local": caller.local,
        "authed": caller.authed,
        "lan": !st.listen.ip().is_loopback(),
    }))
}

#[derive(Deserialize)]
struct AccessBody {
    code: String,
}

async fn enter_access_code(State(st): State<AppState>, Json(body): Json<AccessBody>) -> ApiResult<Response> {
    if let Some(secs) = st.access_failures.lock().unwrap_or_else(|p| p.into_inner()).wait_secs() {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "access_locked", secs.to_string()));
    }
    let code = st.db.run(|c| store::access_code(c)).await?;
    let given = body.code.trim().to_ascii_uppercase();
    let ok = access::same(&given, &code);
    st.access_failures.lock().unwrap_or_else(|p| p.into_inner()).record(ok);
    if !ok {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "access_wrong", "wrong access code"));
    }
    let token = st.access_token.read().unwrap_or_else(|p| p.into_inner()).clone();
    let cookie = format!("{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", access::COOKIE);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

/// 访问码只在运行服务的这台机器上能看、能换。
fn require_local(caller: Caller) -> ApiResult<()> {
    if caller.local {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "local_only", "only available on the host machine"))
    }
}

async fn show_access_code(State(st): State<AppState>, Extension(caller): Extension<Caller>) -> ApiResult<Json<Value>> {
    require_local(caller)?;
    let code = st.db.run(|c| store::access_code(c)).await?;
    Ok(Json(json!({ "code": code, "listen": st.listen.to_string() })))
}

async fn reset_access_code(State(st): State<AppState>, Extension(caller): Extension<Caller>) -> ApiResult<Json<Value>> {
    require_local(caller)?;
    let (code, token) = st.db.run(|c| Ok((store::reset_access_code(c)?, store::access_token(c)?))).await?;
    *st.access_token.write().unwrap_or_else(|p| p.into_inner()) = token;
    Ok(Json(json!({ "code": code, "listen": st.listen.to_string() })))
}

// ---------- 推理后端 ----------

async fn list_providers(State(st): State<AppState>) -> ApiResult<Json<Vec<Provider>>> {
    Ok(Json(st.db.run(|c| store::list_providers(c)).await?))
}

async fn create_provider(State(st): State<AppState>, Json(input): Json<ProviderInput>) -> ApiResult<Json<Provider>> {
    Ok(Json(st.db.run(move |c| store::create_provider(c, input)).await?))
}

async fn update_provider(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ProviderInput>,
) -> ApiResult<Json<Provider>> {
    Ok(Json(st.db.run(move |c| store::update_provider(c, &id, input)).await?))
}

async fn delete_provider(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    st.db.run(move |c| store::delete_provider(c, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn endpoint(p: &Provider) -> Endpoint {
    Endpoint { base_url: p.base_url.clone(), api_key: p.api_key.clone() }
}

/// 连一次后端，返回可用模型和耗时。
async fn check_provider(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let provider = st.db.run(move |c| store::get_provider(c, &id)).await?;
    let started = Instant::now();
    let models = st.llm.list_models(&endpoint(&provider)).await?;
    Ok(Json(json!({ "models": models, "latency_ms": started.elapsed().as_millis() as u64 })))
}

// ---------- 对话 ----------

async fn list_conversations(State(st): State<AppState>) -> ApiResult<Json<Vec<Conversation>>> {
    Ok(Json(st.db.run(|c| store::list_conversations(c)).await?))
}

async fn create_conversation(State(st): State<AppState>) -> ApiResult<Json<Conversation>> {
    Ok(Json(st.db.run(|c| store::create_conversation(c)).await?))
}

async fn get_conversation(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (conversation, messages) =
        st.db.run(move |c| Ok((store::get_conversation(c, &id)?, store::list_messages(c, &id)?))).await?;
    Ok(Json(json!({ "conversation": conversation, "messages": messages })))
}

#[derive(Deserialize)]
struct RenameBody {
    title: String,
}

async fn rename_conversation(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<Conversation>> {
    Ok(Json(st.db.run(move |c| store::rename_conversation(c, &id, &body.title)).await?))
}

async fn delete_conversation(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    st.db.run(move |c| store::delete_conversation(c, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SendBody {
    content: String,
    provider_id: String,
    #[serde(default)]
    model: Option<String>,
}

/// 把保存的消息整理成发给模型的上下文。失败且没有内容的回答不带；
/// 相邻的同一方消息合并，避免个别模型不接受连续两条用户消息。
fn history(messages: &[Message]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    for m in messages.iter().filter(|m| !m.content.trim().is_empty()) {
        match out.last_mut() {
            Some(last) if last.role == m.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&m.content);
            }
            _ => out.push(ChatMessage { role: m.role.clone(), content: m.content.clone() }),
        }
    }
    out
}

fn event(name: &str, data: Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

/// 发一句话，回答以事件流返回：
/// `user`（已保存的提问与对话）→ 若干 `reasoning` / `delta` → `done` 或 `error`（都带已保存的回答）。
/// 浏览器中途断开即视为停止，已生成的部分照样保存。
async fn send_message(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let content = body.content.trim().to_string();
    if content.is_empty() || content.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Error::Invalid("content".into()).into());
    }
    let provider_id = body.provider_id.clone();
    let provider = st.db.run(move |c| store::get_provider(c, &provider_id)).await?;
    let model = body
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| Error::Invalid("model".into()))?;

    let (conv_id, pid, mdl) = (id.clone(), provider.id.clone(), model.clone());
    let (user_msg, conversation, messages) = st
        .db
        .run(move |c| {
            let msg = store::add_user_message(c, &conv_id, &content, &pid, &mdl)?;
            Ok((msg, store::get_conversation(c, &conv_id)?, store::list_messages(c, &conv_id)?))
        })
        .await?;

    let (tx, rx) = mpsc::channel::<Event>(64);
    let _ = tx.send(event("user", json!({ "message": user_msg, "conversation": conversation }))).await;

    tokio::spawn(async move {
        let (mut text, mut reasoning) = (String::new(), String::new());
        let mut failure: Option<Error> = None;
        let mut stopped = false;

        match st.llm.chat_stream(&endpoint(&provider), &model, &history(&messages)).await {
            Err(e) => failure = Some(e),
            Ok(mut stream) => loop {
                tokio::select! {
                    _ = tx.closed() => { stopped = true; break; }
                    item = stream.next() => match item {
                        None => break,
                        Some(Ok(Delta::Content(t))) => {
                            text.push_str(&t);
                            if tx.send(event("delta", json!({ "text": t }))).await.is_err() { stopped = true; break; }
                        }
                        Some(Ok(Delta::Reasoning(t))) => {
                            reasoning.push_str(&t);
                            if tx.send(event("reasoning", json!({ "text": t }))).await.is_err() { stopped = true; break; }
                        }
                        Some(Err(e)) => { failure = Some(e); break; }
                    }
                }
            },
        }

        let status = if failure.is_some() {
            "error"
        } else if stopped {
            "stopped"
        } else {
            "ok"
        };
        let code = failure.as_ref().map(|e| e.code());
        let saved = st.db.run(move |c| store::add_assistant_message(c, &id, &text, &reasoning, status, code)).await;
        let last = match (saved, failure) {
            (Ok(message), None) => event("done", json!({ "message": message })),
            (Ok(message), Some(e)) => {
                event("error", json!({ "code": e.code(), "detail": e.to_string(), "message": message }))
            }
            (Err(e), _) => {
                tracing::error!(error = %e, "could not save the reply");
                event("error", json!({ "code": e.code(), "detail": e.to_string() }))
            }
        };
        let _ = tx.send(last).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            id: String::new(),
            conversation_id: String::new(),
            role: role.into(),
            content: content.into(),
            reasoning: String::new(),
            status: "ok".into(),
            error_code: None,
            created_at: 0,
        }
    }

    #[test]
    fn history_drops_empty_replies_and_merges_neighbours() {
        let h = history(&[msg("user", "一"), msg("assistant", ""), msg("user", "二"), msg("assistant", "答")]);
        assert_eq!(
            h,
            vec![
                ChatMessage { role: "user".into(), content: "一\n\n二".into() },
                ChatMessage { role: "assistant".into(), content: "答".into() },
            ]
        );
    }
}
