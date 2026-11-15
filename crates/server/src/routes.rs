use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use commonbot_core::agent::{self, Agent, AgentInput, AgentSummary};
use commonbot_core::llm::Endpoint;
use commonbot_core::store::{self, Conversation, Provider, ProviderInput};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::access::{self, Caller};
use crate::{ApiError, ApiResult, AppState, VERSION};

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/access", post(enter_access_code))
        .route("/access-code", get(show_access_code).post(reset_access_code))
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/{id}", axum::routing::put(update_provider).delete(delete_provider))
        .route("/providers/{id}/check", post(check_provider))
        .route("/system", get(system_info))
        .route("/system/lan", axum::routing::put(set_lan))
        .route("/settings/web", get(get_web).put(put_web))
        .route("/settings/web/test", post(test_web))
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/prompt-preview", post(preview_prompt))
        .route("/agents/{id}", get(get_agent).put(update_agent).delete(delete_agent))
        .route("/agents/{id}/publish", post(publish_agent).delete(unpublish_agent))
        .route("/agents/{id}/rotate-key", post(rotate_agent_key))
        .nest("/public", crate::published::public())
        .route("/conversations", get(list_conversations).post(create_conversation))
        .route("/conversations/{id}", get(get_conversation).patch(rename_conversation).delete(delete_conversation))
        .merge(crate::chat::routes())
        .merge(crate::routes_kb::routes())
        .merge(crate::builder::routes())
}

// ---------- 基本信息与访问码 ----------

async fn health(State(st): State<AppState>, Extension(caller): Extension<Caller>) -> Json<Value> {
    Json(json!({
        "name": "CommonBot",
        "version": VERSION,
        "local": caller.local,
        "authed": caller.authed,
        "lan": !st.listening().ip().is_loopback(),
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
    Ok(Json(json!({ "code": code, "listen": st.listening().to_string() })))
}

async fn reset_access_code(State(st): State<AppState>, Extension(caller): Extension<Caller>) -> ApiResult<Json<Value>> {
    require_local(caller)?;
    let (code, token) = st.db.run(|c| Ok((store::reset_access_code(c)?, store::access_token(c)?))).await?;
    *st.access_token.write().unwrap_or_else(|p| p.into_inner()) = token;
    Ok(Json(json!({ "code": code, "listen": st.listening().to_string() })))
}

/// 这台机器在局域网里的地址，给其他设备访问用。
fn lan_addresses() -> Vec<String> {
    if_addrs::get_if_addrs()
        .map(|list| {
            let mut v: Vec<String> = list
                .into_iter()
                .filter(|i| !i.is_loopback())
                .filter_map(|i| match i.ip() {
                    std::net::IpAddr::V4(ip) if !ip.is_link_local() => Some(ip.to_string()),
                    _ => None,
                })
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default()
}

async fn system_info(State(st): State<AppState>) -> Json<Value> {
    let addr = st.listening();
    Json(json!({
        "version": VERSION,
        "lan": !addr.ip().is_loopback(),
        "lan_fixed": st.listen_fixed,
        "port": addr.port(),
        "addresses": lan_addresses(),
    }))
}

#[derive(Deserialize)]
struct LanBody {
    enabled: bool,
}

/// 开关局域网访问。只能在运行服务的这台机器上操作；保存后服务按新设置重新监听，不需要重启。
async fn set_lan(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<LanBody>,
) -> ApiResult<Json<Value>> {
    require_local(caller)?;
    if st.listen_fixed {
        return Err(ApiError::new(StatusCode::CONFLICT, "lan_fixed", "the listen address is set by a start-up option"));
    }
    st.db.run(move |c| store::set_setting(c, "lan", if body.enabled { "1" } else { "0" })).await?;
    st.rebind.notify_one();
    Ok(Json(json!({ "lan": body.enabled })))
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

pub(crate) fn endpoint(p: &Provider) -> Endpoint {
    Endpoint { base_url: p.base_url.clone(), api_key: p.api_key.clone() }
}

/// 连一次后端，返回可用模型和耗时。
async fn check_provider(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let provider = st.db.run(move |c| store::get_provider(c, &id)).await?;
    let started = Instant::now();
    let models = st.llm.list_models(&endpoint(&provider)).await?;
    Ok(Json(json!({ "models": models, "latency_ms": started.elapsed().as_millis() as u64 })))
}

// ---------- 智能体 ----------

fn agent_json(a: &Agent) -> Value {
    let mut v = serde_json::to_value(a).unwrap_or_default();
    v["system_prompt"] = json!(a.system_prompt());
    v
}

async fn list_agents(State(st): State<AppState>) -> ApiResult<Json<Vec<AgentSummary>>> {
    Ok(Json(st.db.run(|c| agent::list(c)).await?))
}

async fn create_agent(State(st): State<AppState>, Json(input): Json<AgentInput>) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::create(c, input)).await?)))
}

async fn get_agent(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::get(c, &id)).await?)))
}

async fn update_agent(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<AgentInput>,
) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::update(c, &id, input)).await?)))
}

async fn delete_agent(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    st.db.run(move |c| agent::delete(c, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_agent(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::publish(c, &id)).await?)))
}

async fn unpublish_agent(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::unpublish(c, &id)).await?)))
}

async fn rotate_agent_key(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(agent_json(&st.db.run(move |c| agent::rotate_key(c, &id)).await?)))
}

/// 编辑时预览：按还没保存的定义生成系统提示词。
async fn preview_prompt(Json(input): Json<AgentInput>) -> Json<Value> {
    Json(json!({ "system_prompt": input.system_prompt() }))
}

// ---------- 对话 ----------

#[derive(Deserialize)]
struct AgentRef {
    agent_id: String,
}

async fn list_conversations(
    State(st): State<AppState>,
    Query(q): Query<AgentRef>,
) -> ApiResult<Json<Vec<Conversation>>> {
    Ok(Json(st.db.run(move |c| store::list_conversations(c, &q.agent_id)).await?))
}

async fn create_conversation(State(st): State<AppState>, Json(q): Json<AgentRef>) -> ApiResult<Json<Conversation>> {
    Ok(Json(st.db.run(move |c| store::create_conversation(c, &q.agent_id)).await?))
}

async fn get_conversation(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (conversation, messages, attachments) = st
        .db
        .run(move |c| {
            Ok((store::get_conversation(c, &id)?, store::list_messages(c, &id)?, commonbot_core::attach::list(c, &id)?))
        })
        .await?;
    Ok(Json(json!({ "conversation": conversation, "messages": messages, "attachments": attachments })))
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

// ---------- 联网搜索设置 ----------

async fn get_web(State(st): State<AppState>) -> ApiResult<Json<commonbot_core::web::WebSettings>> {
    Ok(Json(st.db.run(|c| commonbot_core::web::settings(c)).await?))
}

async fn put_web(
    State(st): State<AppState>,
    Json(input): Json<commonbot_core::web::WebSettingsInput>,
) -> ApiResult<Json<commonbot_core::web::WebSettings>> {
    Ok(Json(st.db.run(move |c| commonbot_core::web::save_settings(c, input)).await?))
}

#[derive(Deserialize)]
struct TestWeb {
    query: String,
}

/// 用当前设置搜一次，看看能不能用。
async fn test_web(State(st): State<AppState>, Json(body): Json<TestWeb>) -> ApiResult<Json<Value>> {
    let settings = st.db.run(|c| commonbot_core::web::settings(c)).await?;
    let results = st.web.search(&settings, &body.query, 5).await?;
    Ok(Json(json!({ "results": results })))
}
