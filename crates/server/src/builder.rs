//! 建造师的接口：访谈 → 整理成定义 → 试答 → 发布。

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use commonbot_core::agent::{self, Agent, AgentInput, BUILDER_AGENT_ID};
use commonbot_core::build::{self, Build, TestCase};
use commonbot_core::llm::{extract_json, ChatMessage};
use commonbot_core::store::{self, Provider, Reply};
use commonbot_core::{attach, Error};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::chat::{history, run_turn, Turn, TurnEvent};
use crate::routes::endpoint;
use crate::{ApiResult, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/builds", get(list).post(create))
        .route("/builds/{id}", get(read).delete(remove))
        .route("/builds/{id}/draft", put(edit_draft))
        .route("/builds/{id}/messages", post(say))
        .route("/builds/{id}/test", post(run_tests))
        .route("/builds/{id}/publish", post(publish))
}

fn event(name: &str, data: Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

async fn list(State(st): State<AppState>) -> ApiResult<Json<Vec<Build>>> {
    Ok(Json(st.db.run(|c| build::list(c)).await?))
}

#[derive(Deserialize)]
struct NewBuild {
    /// 建造师的第一句话，按界面当前的语言给出。
    opening: String,
}

async fn create(State(st): State<AppState>, Json(body): Json<NewBuild>) -> ApiResult<Json<Build>> {
    Ok(Json(st.db.run(move |c| build::create(c, &body.opening)).await?))
}

async fn read(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (b, messages, attachments) = st
        .db
        .run(move |c| {
            let b = build::get(c, &id)?;
            let messages = store::list_messages(c, &b.conversation_id)?;
            let attachments = attach::list(c, &b.conversation_id)?;
            Ok((b, messages, attachments))
        })
        .await?;
    Ok(Json(json!({ "build": b, "messages": messages, "attachments": attachments })))
}

async fn remove(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    st.db.run(move |c| build::delete(c, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 用户手动改草稿。改过的字段之后不会被访谈整理覆盖。
async fn edit_draft(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<AgentInput>,
) -> ApiResult<Json<Build>> {
    let b = st
        .db
        .run(move |c| {
            let mut b = build::get(c, &id)?;
            b.draft.apply_manual(input);
            build::save_draft(c, &id, &b.draft)?;
            build::get(c, &id)
        })
        .await?;
    Ok(Json(b))
}

#[derive(Deserialize)]
struct ModelChoice {
    provider_id: String,
    #[serde(default)]
    model: Option<String>,
}

async fn resolve_model(st: &AppState, choice: &ModelChoice) -> ApiResult<(Provider, String)> {
    let pid = choice.provider_id.clone();
    let provider = st.db.run(move |c| store::get_provider(c, &pid)).await?;
    let model = choice
        .model
        .clone()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| Error::Invalid("model".into()))?;
    Ok((provider, model))
}

#[derive(Deserialize)]
struct SayBody {
    content: String,
    #[serde(flatten)]
    model: ModelChoice,
    #[serde(default)]
    attachment_ids: Vec<String>,
}

/// 对建造师说一句话。事件流：`user` → 若干 `delta` → `done`（建造师的回答）→ `draft`（据访谈更新后的草稿）。
/// 整理草稿失败不影响访谈，只发一个 `draft_error`。
async fn say(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SayBody>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let content = body.content.trim().to_string();
    if content.is_empty() || content.chars().count() > 20_000 {
        return Err(Error::Invalid("content".into()).into());
    }
    let (provider, model) = resolve_model(&st, &body.model).await?;
    let (bid, pid, mdl, ids, text) =
        (id.clone(), provider.id.clone(), model.clone(), body.attachment_ids, content.clone());
    let (b, user_msg, messages, files) = st
        .db
        .run(move |c| {
            let b = build::get(c, &bid)?;
            let msg = store::add_user_message(c, &b.conversation_id, &text, &pid, &mdl)?;
            attach::attach_pending(c, &b.conversation_id, &msg.id, &ids)?;
            let messages = store::list_messages(c, &b.conversation_id)?;
            let files = attach::list(c, &b.conversation_id)?;
            Ok((b, msg, messages, files))
        })
        .await?;

    let (tx, rx) = mpsc::channel::<Event>(64);
    let sent: Vec<_> = files.iter().filter(|f| f.message_id.as_deref() == Some(user_msg.id.as_str())).collect();
    let _ = tx.send(event("user", json!({ "message": user_msg, "attachments": sent }))).await;

    // 建造师本身也是一个「智能体」：它的要求里带着当前草稿
    let interviewer = Agent::draft(
        BUILDER_AGENT_ID,
        AgentInput {
            name: "建造师".into(),
            instructions: build::interviewer_instructions(&b.draft),
            ..Default::default()
        },
    );
    let turn = Turn {
        agent: interviewer,
        provider: provider.clone(),
        model: model.clone(),
        history: history(&messages),
        question: content.clone(),
        query: content,
        files,
        folders: Vec::new(),
        web_mode: "off".into(),
    };

    tokio::spawn(async move {
        let (ttx, mut trx) = mpsc::channel::<TurnEvent>(64);
        let worker = {
            let st = st.clone();
            tokio::spawn(async move { run_turn(&st, turn, ttx).await })
        };
        while let Some(ev) = trx.recv().await {
            let e = match ev {
                TurnEvent::Delta(t) => event("delta", json!({ "text": t })),
                TurnEvent::Reasoning(t) => event("reasoning", json!({ "text": t })),
                _ => continue,
            };
            if tx.send(e).await.is_err() {
                break;
            }
        }
        drop(trx);
        let Ok(out) = worker.await else { return };
        let status = if out.failure.is_some() {
            "error"
        } else if out.stopped {
            "stopped"
        } else {
            "ok"
        };
        let code = out.failure.as_ref().map(|e| e.code());
        let (conv, text, reasoning, model_name) = (b.conversation_id.clone(), out.text, out.reasoning, out.model);
        let saved = st
            .db
            .run(move |c| {
                let reply = Reply {
                    content: &text,
                    reasoning: &reasoning,
                    status,
                    error_code: code,
                    model: Some(&model_name),
                    ..Default::default()
                };
                let m = store::add_assistant_message(c, &conv, &reply)?;
                Ok((m, store::list_messages(c, &conv)?))
            })
            .await;
        let all = match (saved, out.failure) {
            (Ok((message, all)), None) => {
                let _ = tx.send(event("done", json!({ "message": message }))).await;
                all
            }
            (Ok((message, _)), Some(e)) => {
                let _ = tx
                    .send(event("error", json!({ "code": e.code(), "detail": e.to_string(), "message": message })))
                    .await;
                return;
            }
            (Err(e), _) => {
                let _ = tx.send(event("error", json!({ "code": e.code(), "detail": e.to_string() }))).await;
                return;
            }
        };
        if out.stopped {
            return;
        }

        // 把到目前为止的访谈整理成定义
        let prompt = build::extract_prompt(&build::transcript(&all));
        let extracted = st
            .llm
            .complete(&endpoint(&provider), &model, &[ChatMessage { role: "user".into(), content: prompt }])
            .await;
        let last = match extracted.map(|text| extract_json(&text)) {
            Ok(Some(v)) if v.is_object() => {
                let updated = st
                    .db
                    .run(move |c| {
                        // 重新读取：访谈期间用户可能手动改过草稿
                        let mut b = build::get(c, &id)?;
                        b.draft.merge_extracted(&v);
                        build::save_draft(c, &id, &b.draft)?;
                        Ok(b.draft)
                    })
                    .await;
                match updated {
                    Ok(draft) => event("draft", json!({ "draft": draft })),
                    Err(e) => event("draft_error", json!({ "code": e.code() })),
                }
            }
            Ok(_) => event("draft_error", json!({ "code": "draft_unreadable" })),
            Err(e) => event("draft_error", json!({ "code": e.code() })),
        };
        let _ = tx.send(last).await;
    });
    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

/// 用草稿试答。事件流：`tests`（题目）→ 每答完一道发 `test`（序号与结果）→ `done`。
async fn run_tests(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(choice): Json<ModelChoice>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let (provider, model) = resolve_model(&st, &choice).await?;
    let bid = id.clone();
    let b = st.db.run(move |c| build::get(c, &bid)).await?;
    if !b.draft.missing().is_empty() {
        return Err(Error::Invalid("draft_incomplete".into()).into());
    }
    let draft = b.draft.clone();
    let validated = draft.input.clone().validated()?;

    let (tx, rx) = mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        let ep = endpoint(&provider);
        let ask = |prompt: String| {
            let (llm, ep, model) = (st.llm.clone(), ep.clone(), model.clone());
            async move { llm.complete(&ep, &model, &[ChatMessage { role: "user".into(), content: prompt }]).await }
        };

        // 出题：模型出不了像样的题时，退回用示例问题
        let mut tests = match ask(build::tests_prompt(&draft)).await {
            Ok(text) => extract_json(&text).map(|v| build::parse_tests(&v)).unwrap_or_default(),
            Err(e) => {
                let _ = tx.send(event("error", json!({ "code": e.code(), "detail": e.to_string() }))).await;
                return;
            }
        };
        if tests.is_empty() {
            tests =
                draft.input.examples.iter().map(|q| TestCase { question: q.clone(), ..Default::default() }).collect();
        }
        if tests.is_empty() {
            let _ = tx.send(event("error", json!({ "code": "tests_unreadable", "detail": "" }))).await;
            return;
        }
        if tx.send(event("tests", json!({ "tests": tests }))).await.is_err() {
            return;
        }

        for i in 0..tests.len() {
            // 用草稿临时构造出这个智能体，按它的知识范围和联网设置来答
            let agent = Agent::draft("draft", validated.clone());
            let turn = Turn {
                history: vec![ChatMessage { role: "user".into(), content: tests[i].question.clone() }],
                question: tests[i].question.clone(),
                query: tests[i].question.clone(),
                files: Vec::new(),
                folders: agent.folders.clone(),
                web_mode: agent.web.clone(),
                agent,
                provider: provider.clone(),
                model: model.clone(),
            };
            let (ttx, mut trx) = mpsc::channel::<TurnEvent>(64);
            let worker = {
                let st = st.clone();
                tokio::spawn(async move { run_turn(&st, turn, ttx).await })
            };
            while trx.recv().await.is_some() {}
            let Ok(out) = worker.await else { return };
            match out.failure {
                Some(e) => {
                    tests[i].pass = None;
                    tests[i].reason = e.code().to_string();
                }
                None => {
                    tests[i].answer = Some(out.text.clone());
                    let verdict = ask(build::judge_prompt(&draft, &tests[i], &out.text))
                        .await
                        .ok()
                        .and_then(|t| extract_json(&t));
                    tests[i].pass = verdict.as_ref().and_then(|v| v["pass"].as_bool());
                    tests[i].reason =
                        verdict.as_ref().and_then(|v| v["reason"].as_str()).unwrap_or("").chars().take(200).collect();
                }
            }
            if tx.send(event("test", json!({ "index": i, "test": tests[i] }))).await.is_err() {
                break; // 界面关了：已答的照样保存
            }
        }
        let (bid, save) = (id.clone(), tests.clone());
        let _ = st.db.run(move |c| build::save_tests(c, &bid, &save)).await;
        let _ = tx.send(event("done", json!({ "tests": tests }))).await;
    });
    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

/// 把草稿发布成正式的智能体，带独立链接和 API 密钥。草稿没固定模型时，用访谈时选的模型。
async fn publish(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(choice): Json<ModelChoice>,
) -> ApiResult<Json<Value>> {
    let (provider, model) = resolve_model(&st, &choice).await?;
    let agent = st
        .db
        .run(move |c| {
            let b = build::get(c, &id)?;
            if !b.draft.missing().is_empty() {
                return Err(Error::Invalid("draft_incomplete".into()));
            }
            let mut input = b.draft.input.clone();
            if input.model.is_none() || input.provider_id.is_none() {
                input.provider_id = Some(provider.id.clone());
                input.model = Some(model.clone());
            }
            // 同一次建造再次发布：更新已有的智能体，不重复创建
            let created = match b.agent_id.as_deref().filter(|aid| agent::get(c, aid).is_ok()) {
                Some(aid) => agent::update(c, aid, input)?,
                None => agent::create(c, input)?,
            };
            build::set_agent(c, &id, &created.id)?;
            agent::publish(c, &created.id)
        })
        .await?;
    let mut v = serde_json::to_value(&agent).unwrap_or_default();
    v["system_prompt"] = json!(agent.system_prompt());
    Ok(Json(v))
}
