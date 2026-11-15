//! 内嵌的界面文件。

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::ApiError;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Dist;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such endpoint").into_response();
    }
    match Dist::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // 文件名带内容摘要的资源可以长期缓存；其余每次确认，升级后不会用到旧文件。
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data)
                .into_response()
        }
        // 界面内的页面地址（如 /settings/models）都交给首页处理。
        None => match Dist::get("index.html") {
            Some(index) => {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], index.data)
                    .into_response()
            }
            None => (StatusCode::NOT_FOUND, "interface files are missing").into_response(),
        },
    }
}
