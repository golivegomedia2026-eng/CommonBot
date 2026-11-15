//! 接口测试：真实的路由 + 内存数据库 + 假推理后端。

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use commonbot_core::Db;
use commonbot_mock_llm::MockConfig;
use commonbot_server::{app, AppState};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LOCAL: &str = "127.0.0.1:50000";
const REMOTE: &str = "192.168.1.20:50000";

struct Resp {
    status: StatusCode,
    cookie: Option<String>,
    text: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not json ({e}): {}", self.text))
    }
    /// 事件流里的事件：(名称, 数据)。
    fn events(&self) -> Vec<(String, Value)> {
        self.text
            .split("\n\n")
            .filter_map(|block| {
                let name = block.lines().find_map(|l| l.strip_prefix("event: "))?;
                let data = block.lines().find_map(|l| l.strip_prefix("data: "))?;
                Some((name.to_string(), serde_json::from_str(data).ok()?))
            })
            .collect()
    }
}

struct Req<'a> {
    method: Method,
    path: &'a str,
    body: Option<Value>,
    peer: &'a str,
    host: &'a str,
    cookie: Option<&'a str>,
    origin: Option<&'a str>,
}

fn req<'a>(method: Method, path: &'a str) -> Req<'a> {
    Req { method, path, body: None, peer: LOCAL, host: "localhost:8600", cookie: None, origin: None }
}

impl<'a> Req<'a> {
    fn json(mut self, v: Value) -> Self {
        self.body = Some(v);
        self
    }
    fn remote(mut self) -> Self {
        self.peer = REMOTE;
        self.host = "192.168.1.5:8600";
        self
    }
    async fn send(self, app: &Router) -> Resp {
        let mut b = Request::builder().method(self.method).uri(self.path).header(header::HOST, self.host);
        if let Some(c) = self.cookie {
            b = b.header(header::COOKIE, c);
        }
        if let Some(o) = self.origin {
            b = b.header(header::ORIGIN, o);
        }
        let body = match self.body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut request = b.body(body).unwrap();
        request.extensions_mut().insert(ConnectInfo(self.peer.parse::<SocketAddr>().unwrap()));
        let resp = app.clone().oneshot(request).await.unwrap();
        let status = resp.status();
        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_string);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        Resp { status, cookie, text: String::from_utf8_lossy(&bytes).into_owned() }
    }
}

async fn setup() -> Router {
    let db = Db::open_in_memory().unwrap();
    app(AppState::new(db, "0.0.0.0:8600".parse().unwrap()).await.unwrap())
}

async fn add_provider(app: &Router, base_url: &str, key: Option<&str>) -> String {
    let r = req(Method::POST, "/api/providers")
        .json(json!({ "name": "测试后端", "kind": "local", "base_url": base_url, "api_key": key, "default_model": "mock-small" }))
        .send(app)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn first_run_has_a_default_backend_and_reports_version() {
    let app = setup().await;
    let h = req(Method::GET, "/api/health").send(&app).await.json();
    assert_eq!(h["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(h["local"], true);
    let list = req(Method::GET, "/api/providers").send(&app).await.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["base_url"], "http://127.0.0.1:11434/v1");
}

#[tokio::test]
async fn checks_backend_and_classifies_failures() {
    let app = setup().await;
    let good = commonbot_mock_llm::spawn(MockConfig { api_key: Some("sk-ok".into()), ..Default::default() }).await;

    let id = add_provider(&app, &good, Some("sk-ok")).await;
    let r = req(Method::POST, &format!("/api/providers/{id}/check")).send(&app).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["models"], json!(["mock-large", "mock-small"]));

    // 密钥不对
    let id = add_provider(&app, &good, Some("sk-wrong")).await;
    let r = req(Method::POST, &format!("/api/providers/{id}/check")).send(&app).await;
    assert_eq!(
        (r.status, r.json()["code"].as_str().unwrap().to_string()),
        (StatusCode::BAD_GATEWAY, "model_unauthorized".into())
    );

    // 没有服务在听
    let id = add_provider(&app, "http://127.0.0.1:9/v1", None).await;
    let r = req(Method::POST, &format!("/api/providers/{id}/check")).send(&app).await;
    assert_eq!(r.json()["code"], "model_unreachable");

    // 密钥不出现在任何返回里
    let list = req(Method::GET, "/api/providers").send(&app).await;
    assert!(!list.text.contains("sk-ok") && !list.text.contains("sk-wrong"), "{}", list.text);
}

#[tokio::test]
async fn streams_a_reply_and_keeps_the_conversation() {
    let app = setup().await;
    let base = commonbot_mock_llm::spawn(MockConfig::default()).await;
    let pid = add_provider(&app, &base, None).await;
    let conv = req(Method::POST, "/api/conversations").send(&app).await.json();
    let cid = conv["id"].as_str().unwrap();
    let path = format!("/api/conversations/{cid}/messages");

    let r =
        req(Method::POST, &path).json(json!({ "content": "你好，介绍一下自己", "provider_id": pid })).send(&app).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let events = r.events();
    assert_eq!(events.first().unwrap().0, "user");
    assert_eq!(events.last().unwrap().0, "done");
    let streamed: String =
        events.iter().filter(|(n, _)| n == "delta").map(|(_, d)| d["text"].as_str().unwrap()).collect();
    assert!(streamed.contains("你说的是：**你好，介绍一下自己**"), "{streamed}");
    assert!(events.iter().filter(|(n, _)| n == "delta").count() > 5, "reply should arrive in pieces");
    let saved = &events.last().unwrap().1["message"];
    assert_eq!(saved["content"].as_str().unwrap(), streamed);
    assert_eq!(saved["reasoning"], "先想一想。");

    // 第二句话带上了前文
    let r = req(Method::POST, &path)
        .json(json!({ "content": "再说一遍", "provider_id": pid, "model": "mock-large" }))
        .send(&app)
        .await;
    let reply = r.events().last().unwrap().1["message"]["content"].as_str().unwrap().to_string();
    assert!(reply.contains("mock-large") && reply.contains("带了 3 条消息"), "{reply}");

    // 重新读取：四条消息、标题取自第一句
    let got = req(Method::GET, &format!("/api/conversations/{cid}")).send(&app).await.json();
    assert_eq!(got["messages"].as_array().unwrap().len(), 4);
    assert_eq!(got["conversation"]["title"], "你好，介绍一下自己");
    assert_eq!(got["conversation"]["model"], "mock-large");
    let list = req(Method::GET, "/api/conversations").send(&app).await.json();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 删除后读不到
    assert_eq!(
        req(Method::DELETE, &format!("/api/conversations/{cid}")).send(&app).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(req(Method::GET, &format!("/api/conversations/{cid}")).send(&app).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn model_failure_is_reported_and_saved() {
    let app = setup().await;
    let base = commonbot_mock_llm::spawn(MockConfig::default()).await;
    let pid = add_provider(&app, &base, None).await;
    let cid = req(Method::POST, "/api/conversations").send(&app).await.json()["id"].as_str().unwrap().to_string();
    let path = format!("/api/conversations/{cid}/messages");

    let r = req(Method::POST, &path)
        .json(json!({ "content": "在吗", "provider_id": pid, "model": "missing" }))
        .send(&app)
        .await;
    let events = r.events();
    let (name, data) = events.last().unwrap();
    assert_eq!(name, "error");
    assert_eq!(data["code"], "model_error");
    assert!(data["detail"].as_str().unwrap().contains("not found"));
    assert_eq!(data["message"]["status"], "error");

    // 输入不合法时不建立事件流，直接报错
    for body in [json!({ "content": "  ", "provider_id": pid }), json!({ "content": "x", "provider_id": "nope" })] {
        let r = req(Method::POST, &path).json(body).send(&app).await;
        assert!(r.status.is_client_error(), "{}", r.text);
    }
}

#[tokio::test]
async fn other_devices_need_the_access_code() {
    let app = setup().await;

    // 界面文件和健康检查不需要访问码，其余接口需要
    assert_eq!(req(Method::GET, "/").remote().send(&app).await.status, StatusCode::OK);
    let h = req(Method::GET, "/api/health").remote().send(&app).await.json();
    assert_eq!((h["local"].clone(), h["authed"].clone()), (json!(false), json!(false)));
    let r = req(Method::GET, "/api/providers").remote().send(&app).await;
    assert_eq!(
        (r.status, r.json()["code"].as_str().unwrap().to_string()),
        (StatusCode::UNAUTHORIZED, "access_required".into())
    );

    // 访问码只能在本机查看
    assert_eq!(req(Method::GET, "/api/access-code").remote().send(&app).await.status, StatusCode::UNAUTHORIZED);
    let code = req(Method::GET, "/api/access-code").send(&app).await.json()["code"].as_str().unwrap().to_string();

    // 输错被拒；输对拿到凭据，小写也接受
    let r = req(Method::POST, "/api/access").remote().json(json!({ "code": "WRONG123" })).send(&app).await;
    assert_eq!(r.json()["code"], "access_wrong");
    let r = req(Method::POST, "/api/access").remote().json(json!({ "code": code.to_lowercase() })).send(&app).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let cookie = r.cookie.expect("cookie");

    let mut with_cookie = req(Method::GET, "/api/providers").remote();
    with_cookie.cookie = Some(&cookie);
    assert_eq!(with_cookie.send(&app).await.status, StatusCode::OK);

    // 已登录的其他设备仍然看不到访问码
    let mut peek = req(Method::GET, "/api/access-code").remote();
    peek.cookie = Some(&cookie);
    assert_eq!(peek.send(&app).await.json()["code"], "local_only");

    // 本机换访问码后，旧凭据失效
    assert_eq!(req(Method::POST, "/api/access-code").send(&app).await.status, StatusCode::OK);
    let mut stale = req(Method::GET, "/api/providers").remote();
    stale.cookie = Some(&cookie);
    assert_eq!(stale.send(&app).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_wrong_codes_are_locked_out() {
    let app = setup().await;
    for _ in 0..5 {
        let r = req(Method::POST, "/api/access").remote().json(json!({ "code": "NOPE0000" })).send(&app).await;
        assert_eq!(r.json()["code"], "access_wrong");
    }
    let code = req(Method::GET, "/api/access-code").send(&app).await.json()["code"].as_str().unwrap().to_string();
    let r = req(Method::POST, "/api/access").remote().json(json!({ "code": code })).send(&app).await;
    assert_eq!(
        (r.status, r.json()["code"].as_str().unwrap().to_string()),
        (StatusCode::TOO_MANY_REQUESTS, "access_locked".into())
    );
}

#[tokio::test]
async fn refuses_cross_site_writes_and_disguised_local_hosts() {
    let app = setup().await;

    // 别的网页借本机浏览器发来的写请求
    let mut r = req(Method::POST, "/api/conversations");
    r.origin = Some("https://evil.example");
    let r = r.send(&app).await;
    assert_eq!(
        (r.status, r.json()["code"].as_str().unwrap().to_string()),
        (StatusCode::FORBIDDEN, "bad_origin".into())
    );

    // 同源的写请求正常
    let mut r = req(Method::POST, "/api/conversations");
    r.origin = Some("http://localhost:8600");
    assert_eq!(r.send(&app).await.status, StatusCode::OK);

    // 连接来自本机，但地址栏不是本机地址（域名被指到 127.0.0.1）：不当作本机
    let mut r = req(Method::GET, "/api/providers");
    r.host = "attacker.example:8600";
    assert_eq!(r.send(&app).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_api_path_is_json_404_and_pages_fall_back_to_index() {
    let app = setup().await;
    let r = req(Method::GET, "/api/nope").send(&app).await;
    assert_eq!((r.status, r.json()["code"].as_str().unwrap().to_string()), (StatusCode::NOT_FOUND, "not_found".into()));
    let r = req(Method::GET, "/settings/models").send(&app).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.to_lowercase().contains("<!doctype html"));
}
