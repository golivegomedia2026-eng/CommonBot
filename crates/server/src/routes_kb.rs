//! 知识库接口：文件夹、文档上传与处理、检索、相关设置。

use std::path::Path as FsPath;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use commonbot_core::kb::{self, Document};
use commonbot_core::{new_id, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::{ApiError, ApiResult, AppState};

/// 单个文件的大小上限。
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// 一次上传（可含多个文件）的总大小上限。
const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/kb/folders", get(list_folders).post(create_folder))
        .route("/kb/folders/{id}", axum::routing::patch(update_folder).delete(delete_folder))
        .route(
            "/kb/folders/{id}/documents",
            get(list_documents).post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/kb/documents/{id}", axum::routing::delete(delete_document).patch(move_document))
        .route("/kb/documents/{id}/chunks", get(list_chunks))
        .route("/kb/documents/{id}/{action}", post(document_action))
        .route("/kb/search", post(search))
        .route("/kb/settings", get(get_settings).put(put_settings))
}

async fn list_folders(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let (folders, stats) = st.db.run(|c| Ok((kb::list_folders(c)?, kb::stats(c, None)?))).await?;
    Ok(Json(json!({ "folders": folders, "stats": stats })))
}

#[derive(Deserialize)]
struct NewFolder {
    name: String,
    #[serde(default)]
    parent_id: Option<String>,
}

async fn create_folder(State(st): State<AppState>, Json(body): Json<NewFolder>) -> ApiResult<Json<Value>> {
    let id = st.db.run(move |c| kb::create_folder(c, &body.name, body.parent_id.as_deref())).await?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct EditFolder {
    #[serde(default)]
    name: Option<String>,
    /// 为 true 时移动到 `parent_id`（不带或为 null 表示移到最上层）。
    #[serde(default, rename = "move")]
    do_move: bool,
    #[serde(default)]
    parent_id: Option<String>,
}

async fn update_folder(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EditFolder>,
) -> ApiResult<StatusCode> {
    st.db
        .run(move |c| {
            let parent = body.do_move.then_some(body.parent_id.as_deref());
            kb::update_folder(c, &id, body.name.as_deref(), parent)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_folder(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let docs = st.db.run(move |c| kb::delete_folder(c, &id)).await?;
    st.retriever.index.invalidate();
    st.ingestor.remove_files(docs).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    recursive: bool,
}

async fn list_documents(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let (documents, stats) = st
        .db
        .run(move |c| {
            let documents = kb::list_documents(c, &id, q.recursive)?;
            let scope = if q.recursive { kb::subtree(c, std::slice::from_ref(&id))? } else { vec![id] };
            Ok((documents, kb::stats(c, Some(&scope))?))
        })
        .await?;
    Ok(Json(json!({ "documents": documents, "stats": stats })))
}

/// 只取文件名本身，丢掉浏览器可能带上的路径。
fn base_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).trim().to_string()
}

/// 上传一个或多个文件。不支持的类型和过大的文件逐个报告，不影响其余文件。
async fn upload(
    State(st): State<AppState>,
    Path(folder_id): Path<String>,
    mut form: Multipart,
) -> ApiResult<Json<Value>> {
    // 先确认文件夹存在，免得白传
    let fid = folder_id.clone();
    st.db.run(move |c| kb::list_documents(c, &fid, false).map(|_| ())).await?;

    let mut added: Vec<Document> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    while let Some(mut field) =
        form.next_field().await.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "upload_failed", e.to_string()))?
    {
        let Some(name) = field.file_name().map(base_name).filter(|n| !n.is_empty()) else { continue };
        let ext = kb::ext_of(&name);
        if kb::parse::kind_of(&ext).is_none() {
            rejected.push(json!({ "name": name, "code": "doc_unsupported" }));
            continue;
        }
        let id = new_id();
        let path = st.ingestor.file_path(&id, &ext);
        match save(&mut field, &path).await {
            Ok(size) => {
                let (doc_id, fid, n) = (id.clone(), folder_id.clone(), name.clone());
                match st.db.run(move |c| kb::add_document(c, &doc_id, &fid, &n, size as i64)).await {
                    Ok(doc) => added.push(doc),
                    Err(e) => {
                        st.ingestor.remove_files(vec![id]).await;
                        rejected.push(json!({ "name": name, "code": e.code() }));
                    }
                }
            }
            Err(code) => {
                st.ingestor.remove_files(vec![id]).await;
                rejected.push(json!({ "name": name, "code": code }));
            }
        }
    }
    if !added.is_empty() {
        st.ingestor.kick();
    }
    Ok(Json(json!({ "added": added, "rejected": rejected })))
}

async fn save(field: &mut axum::extract::multipart::Field<'_>, path: &FsPath) -> Result<u64, &'static str> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|_| "storage")?;
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|_| "storage")?;
    let mut size = 0u64;
    while let Some(chunk) = field.chunk().await.map_err(|_| "upload_failed")? {
        size += chunk.len() as u64;
        if size > MAX_FILE_BYTES {
            return Err("doc_too_large");
        }
        file.write_all(&chunk).await.map_err(|_| "storage")?;
    }
    file.flush().await.map_err(|_| "storage")?;
    if size == 0 {
        return Err("doc_empty");
    }
    Ok(size)
}

async fn delete_document(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let doc_id = id.clone();
    st.db.run(move |c| kb::delete_document(c, &doc_id)).await?;
    st.retriever.index.invalidate();
    st.ingestor.remove_files(vec![id]).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MoveBody {
    folder_id: String,
}

async fn move_document(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MoveBody>,
) -> ApiResult<StatusCode> {
    st.db.run(move |c| kb::move_document(c, &id, &body.folder_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn document_action(
    State(st): State<AppState>,
    Path((id, action)): Path<(String, String)>,
) -> ApiResult<Json<Document>> {
    let doc = st
        .db
        .run(move |c| {
            match action.as_str() {
                "retry" => kb::requeue(c, &id, true)?,
                "pause" => kb::pause(c, &id)?,
                "resume" => kb::resume(c, &id)?,
                _ => return Err(Error::NotFound("action")),
            }
            kb::get_document(c, &id)
        })
        .await?;
    st.ingestor.kick();
    Ok(Json(doc))
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    50
}

async fn list_chunks(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(p): Query<Page>,
) -> ApiResult<Json<Value>> {
    let chunks = st.db.run(move |c| kb::list_chunks(c, &id, p.offset, p.limit)).await?;
    Ok(Json(json!({ "chunks": chunks })))
}

#[derive(Deserialize)]
struct SearchBody {
    query: String,
    folder_ids: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// 检索测试：看一个问题会找到哪些片段。
async fn search(State(st): State<AppState>, Json(body): Json<SearchBody>) -> ApiResult<Json<Value>> {
    let found = st.retriever.retrieve(&body.query, &body.folder_ids, body.limit.unwrap_or(8).clamp(1, 30)).await?;
    Ok(Json(serde_json::to_value(found).unwrap_or_default()))
}

async fn settings_json(st: &AppState) -> ApiResult<Value> {
    let models = st.db.run(|c| kb::models(c)).await?;
    let pick = |m: &Option<(commonbot_core::store::Provider, String)>| {
        m.as_ref().map(|(p, model)| json!({ "provider_id": p.id, "model": model }))
    };
    Ok(json!({
        "embed": pick(&models.embed),
        "ocr": pick(&models.ocr),
        "pdf_engine": st.ingestor.pdfium.is_some(),
    }))
}

async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(settings_json(&st).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SettingsBody {
    embed_provider_id: String,
    embed_model: String,
    ocr_provider_id: String,
    ocr_model: String,
}

async fn put_settings(State(st): State<AppState>, Json(b): Json<SettingsBody>) -> ApiResult<Json<Value>> {
    let embed_changed = st
        .db
        .run(move |c| {
            let embed_changed = kb::set_model(c, "embed", &b.embed_provider_id, &b.embed_model)?;
            let ocr_changed = kb::set_model(c, "ocr", &b.ocr_provider_id, &b.ocr_model)?;
            if embed_changed {
                // 换了向量化模型，旧向量不能和新向量比较：全部重算
                kb::requeue_all_for_embedding(c)?;
            }
            if ocr_changed && !b.ocr_model.trim().is_empty() {
                kb::requeue_waiting_for_ocr(c)?;
            }
            Ok(embed_changed)
        })
        .await?;
    if embed_changed {
        st.retriever.index.invalidate();
    }
    st.ingestor.kick();
    Ok(Json(settings_json(&st).await?))
}
