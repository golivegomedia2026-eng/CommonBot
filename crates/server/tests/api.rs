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

fn pdfium() -> Option<std::path::PathBuf> {
    let vendor = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium");
    let dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&vendor)
        .map(|d| d.flatten().flat_map(|e| [e.path().join("lib"), e.path().join("bin")]).collect())
        .unwrap_or_default();
    commonbot_core::kb::ingest::find_pdfium(&dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>())
}

async fn setup() -> Router {
    let db = Db::open_in_memory().unwrap();
    // 每个测试用自己的临时目录存上传的文件；目录在测试进程结束时由系统清理
    let dir = std::env::temp_dir().join(format!("commonbot-test-{}", commonbot_core::new_id()));
    app(AppState::new(db, "0.0.0.0:8600".parse().unwrap(), dir, pdfium()).await.unwrap())
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
    assert_eq!(r.json()["models"], json!(["mock-embed", "mock-large", "mock-small", "mock-vision"]));

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
    let conv = req(Method::POST, "/api/conversations").json(json!({ "agent_id": "general" })).send(&app).await.json();
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
    assert!(reply.contains("mock-large") && reply.contains("带了 4 条消息"), "{reply}");

    // 重新读取：四条消息、标题取自第一句
    let got = req(Method::GET, &format!("/api/conversations/{cid}")).send(&app).await.json();
    assert_eq!(got["messages"].as_array().unwrap().len(), 4);
    assert_eq!(got["conversation"]["title"], "你好，介绍一下自己");
    assert_eq!(got["conversation"]["model"], "mock-large");
    let list = req(Method::GET, "/api/conversations?agent_id=general").send(&app).await.json();
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
    let cid = req(Method::POST, "/api/conversations").json(json!({ "agent_id": "general" })).send(&app).await.json()
        ["id"]
        .as_str()
        .unwrap()
        .to_string();
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
    let mut r = req(Method::POST, "/api/conversations").json(json!({ "agent_id": "general" }));
    r.origin = Some("https://evil.example");
    let r = r.send(&app).await;
    assert_eq!(
        (r.status, r.json()["code"].as_str().unwrap().to_string()),
        (StatusCode::FORBIDDEN, "bad_origin".into())
    );

    // 同源的写请求正常
    let mut r = req(Method::POST, "/api/conversations").json(json!({ "agent_id": "general" }));
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

#[tokio::test]
async fn agents_answer_under_their_own_definition_and_model() {
    let app = setup().await;
    let base = commonbot_mock_llm::spawn(MockConfig::default()).await;
    let pid = add_provider(&app, &base, None).await;

    // 自带通用助手
    let list = req(Method::GET, "/api/agents").send(&app).await.json();
    assert_eq!(list[0]["id"], "general");

    // 预览还没保存的定义
    let draft = json!({ "name": "售后答疑", "duty": "回答保修和退换货问题", "out_of_scope": "不报价", "examples": ["保修多久？"] });
    let preview = req(Method::POST, "/api/agents/prompt-preview").json(draft.clone()).send(&app).await.json();
    assert!(preview["system_prompt"].as_str().unwrap().contains("## 不在你负责范围内\n不报价"));

    // 新建两个职责不同的智能体，其中一个固定模型
    let a = req(Method::POST, "/api/agents").json(draft).send(&app).await.json();
    assert_eq!(a["examples"], json!(["保修多久？"]));
    let b = req(Method::POST, "/api/agents")
        .json(json!({ "name": "招标文件助手", "duty": "解读招标文件", "provider_id": pid, "model": "mock-large" }))
        .send(&app)
        .await
        .json();
    let (aid, bid) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());

    let ask = |agent_id: String, body: Value| {
        let app = app.clone();
        async move {
            let cid =
                req(Method::POST, "/api/conversations").json(json!({ "agent_id": agent_id })).send(&app).await.json()
                    ["id"]
                    .as_str()
                    .unwrap()
                    .to_string();
            let r = req(Method::POST, &format!("/api/conversations/{cid}/messages")).json(body).send(&app).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
            r.events().last().unwrap().1["message"]["content"].as_str().unwrap().to_string()
        }
    };

    // 各自带着自己的系统提示词
    let reply = ask(aid.to_string(), json!({ "content": "保修多久", "provider_id": pid })).await;
    assert!(reply.contains("系统提示词开头：你是「售后答疑」") && reply.contains("mock-small"), "{reply}");
    // 固定了模型的智能体不需要、也不理会对话里选的模型
    let reply = ask(bid.to_string(), json!({ "content": "截止时间", "provider_id": pid, "model": "mock-small" })).await;
    assert!(reply.contains("系统提示词开头：你是「招标文件助手」") && reply.contains("mock-large"), "{reply}");
    let reply = ask(bid.to_string(), json!({ "content": "资质要求" })).await;
    assert!(reply.contains("mock-large"), "{reply}");
    // 没固定模型又没选模型：直接报错
    let cid = req(Method::POST, "/api/conversations").json(json!({ "agent_id": aid })).send(&app).await.json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = req(Method::POST, &format!("/api/conversations/{cid}/messages"))
        .json(json!({ "content": "x" }))
        .send(&app)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // 对话按智能体分开；列表带使用次数
    let convs = req(Method::GET, &format!("/api/conversations?agent_id={bid}")).send(&app).await.json();
    assert_eq!(convs.as_array().unwrap().len(), 2);
    let list = req(Method::GET, "/api/agents").send(&app).await.json();
    let b_row = list.as_array().unwrap().iter().find(|x| x["id"] == bid).unwrap();
    assert_eq!(b_row["conversations"], 2);

    // 改定义后立即生效
    let r = req(Method::PUT, &format!("/api/agents/{aid}")).json(json!({ "name": "售后二线" })).send(&app).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let reply = ask(aid.to_string(), json!({ "content": "在吗", "provider_id": pid })).await;
    assert!(reply.contains("你是「售后二线」"), "{reply}");

    // 固定的推理服务被删后，退回到对话里选的
    let other = add_provider(&app, &base, None).await;
    assert_eq!(req(Method::DELETE, &format!("/api/providers/{pid}")).send(&app).await.status, StatusCode::NO_CONTENT);
    let reply = ask(bid.to_string(), json!({ "content": "还在吗", "provider_id": other })).await;
    assert!(reply.contains("mock-small"), "{reply}");

    // 删除智能体连同对话；名称不合法被拒
    assert_eq!(req(Method::DELETE, &format!("/api/agents/{bid}")).send(&app).await.status, StatusCode::NO_CONTENT);
    assert_eq!(req(Method::GET, &format!("/api/conversations?agent_id={bid}")).send(&app).await.json(), json!([]));
    assert_eq!(
        req(Method::POST, "/api/agents").json(json!({ "name": " " })).send(&app).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        req(Method::POST, "/api/conversations").json(json!({ "agent_id": bid })).send(&app).await.status,
        StatusCode::NOT_FOUND
    );
}

// ---------- 知识库 ----------

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/fixtures").join(name)).unwrap()
}

/// 以表单方式上传若干文件。
async fn upload(app: &Router, folder: &str, files: &[(&str, Vec<u8>)]) -> Value {
    let boundary = "----commonbot-test";
    let mut body: Vec<u8> = Vec::new();
    for (name, bytes) in files {
        body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend(bytes);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{boundary}--\r\n").as_bytes());
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/kb/folders/{folder}/documents"))
        .header(header::HOST, "localhost:8600")
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
    let resp = app.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// 等队列处理完，返回文件夹里的文档（按名称索引）。
async fn settle(app: &Router, folder: &str) -> std::collections::HashMap<String, Value> {
    for _ in 0..200 {
        let r = req(Method::GET, &format!("/api/kb/folders/{folder}/documents?recursive=true")).send(app).await.json();
        let docs = r["documents"].as_array().unwrap().clone();
        if docs.iter().all(|d| matches!(d["status"].as_str().unwrap(), "ready" | "failed" | "paused")) {
            return docs.into_iter().map(|d| (d["name"].as_str().unwrap().to_string(), d)).collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("documents did not finish processing");
}

async fn folder(app: &Router, name: &str, parent: Option<&str>) -> String {
    let r = req(Method::POST, "/api/kb/folders").json(json!({ "name": name, "parent_id": parent })).send(app).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json()["id"].as_str().unwrap().to_string()
}

async fn search(app: &Router, query: &str, folders: &[&str]) -> Value {
    req(Method::POST, "/api/kb/search").json(json!({ "query": query, "folder_ids": folders })).send(app).await.json()
}

#[tokio::test]
async fn knowledge_base_end_to_end() {
    let app = setup().await;
    let base = commonbot_mock_llm::spawn(MockConfig::default()).await;
    let pid = add_provider(&app, &base, None).await;

    let sales = folder(&app, "售后资料", None).await;
    let bids = folder(&app, "招标文件", None).await;
    let bids_2026 = folder(&app, "2026 政务云", Some(&bids)).await;

    // 1. 还没配置任何模型：文字类文档照常入库，只能按关键词查；纯扫描件报「需要文字识别模型」
    let up = upload(
        &app,
        &sales,
        &[
            ("保修条款.pdf", fixture("text.pdf")),
            ("售后手册.docx", fixture("sample.docx")),
            ("延保说明.txt", fixture("gbk.txt")),
            ("压缩包.zip", b"PK".to_vec()),
        ],
    )
    .await;
    assert_eq!(up["added"].as_array().unwrap().len(), 3);
    assert_eq!(up["rejected"], json!([{ "name": "压缩包.zip", "code": "doc_unsupported" }]));
    upload(
        &app,
        &bids_2026,
        &[
            ("评分办法（扫描）.pdf", fixture("scan.pdf")),
            ("招标说明.pdf", fixture("mixed.pdf")),
            ("坏文件.pdf", fixture("broken.pdf")),
        ],
    )
    .await;
    upload(&app, &bids, &[("历年中标.xlsx", fixture("sample.xlsx"))]).await;

    let sales_docs = settle(&app, &sales).await;
    assert!(sales_docs.values().all(|d| d["status"] == "ready" && d["embed_model"].is_null()), "{sales_docs:?}");
    assert_eq!(sales_docs["保修条款.pdf"]["pages"], 2);
    let bid_docs = settle(&app, &bids).await;
    assert_eq!(bid_docs["评分办法（扫描）.pdf"]["error_code"], "ocr_not_configured");
    assert_eq!(bid_docs["坏文件.pdf"]["error_code"], "doc_damaged");
    assert_eq!(
        (
            bid_docs["招标说明.pdf"]["status"].as_str().unwrap(),
            bid_docs["招标说明.pdf"]["ocr_skipped"].as_i64().unwrap()
        ),
        ("ready", 1)
    );

    let found = search(&app, "屏幕保修多久", &[&sales]).await;
    assert_eq!(found["semantic"], false);
    assert_eq!(found["note"], "embed_not_configured");
    let top = &found["hits"][0];
    assert_eq!((top["document"].as_str().unwrap(), top["folder"].as_str().unwrap()), ("保修条款.pdf", "售后资料"));
    assert_eq!(top["locator"], json!({ "kind": "page", "value": 1 }));
    // GBK 文本、Word 表格、Excel 都搜得到
    assert_eq!(search(&app, "延保服务价格", &[&sales]).await["hits"][0]["document"], "延保说明.txt");
    assert!(search(&app, "换屏多少钱", &[&sales]).await["hits"][0]["text"].as_str().unwrap().contains("换屏 | 600 元"));
    assert_eq!(
        search(&app, "智慧园区中标金额", &[&bids]).await["hits"][0]["locator"],
        json!({ "kind": "sheet", "value": "中标公告" })
    );
    // 范围：售后的内容在招标文件夹里搜不到；父文件夹包含子文件夹
    assert_eq!(search(&app, "屏幕保修", &[&bids]).await["hits"], json!([]));
    assert_eq!(search(&app, "投标截止时间", &[&bids]).await["hits"][0]["document"], "招标说明.pdf");
    // 扫描页的内容此时还搜不到
    assert_eq!(search(&app, "技术分", &[&bids]).await["hits"], json!([]));

    // 2. 配置文字识别模型：之前等它的文档自动重新处理
    let s = req(Method::PUT, "/api/kb/settings")
        .json(json!({ "ocr_provider_id": pid, "ocr_model": "mock-vision" }))
        .send(&app)
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text);
    assert_eq!(s.json()["pdf_engine"], true);
    let bid_docs = settle(&app, &bids).await;
    assert_eq!(
        (
            bid_docs["评分办法（扫描）.pdf"]["status"].as_str().unwrap(),
            bid_docs["评分办法（扫描）.pdf"]["ocr_pages"].as_i64().unwrap()
        ),
        ("ready", 1)
    );
    assert_eq!(
        (
            bid_docs["招标说明.pdf"]["ocr_pages"].as_i64().unwrap(),
            bid_docs["招标说明.pdf"]["ocr_skipped"].as_i64().unwrap()
        ),
        (1, 0)
    );
    let hit = &search(&app, "技术分占多少", &[&bids_2026]).await["hits"][0];
    assert!(hit["text"].as_str().unwrap().contains("技术分占百分之六十"), "{hit}");
    assert!(!hit["text"].as_str().unwrap().contains("think"), "thinking text must be stripped: {hit}");
    // 图片也能入库
    upload(&app, &bids_2026, &[("现场照片.png", fixture("scan.png"))]).await;
    assert_eq!(settle(&app, &bids).await["现场照片.png"]["status"], "ready");

    // 3. 配置向量化模型：已入库文档只重新向量化（不重新识别），之后用上语义检索
    req(Method::PUT, "/api/kb/settings")
        .json(json!({ "embed_provider_id": pid, "embed_model": "mock-embed", "ocr_provider_id": pid, "ocr_model": "mock-vision" }))
        .send(&app)
        .await;
    let sales_docs = settle(&app, &sales).await;
    assert!(sales_docs.values().all(|d| d["embed_model"] == "mock-embed"), "{sales_docs:?}");
    settle(&app, &bids).await;
    let found = search(&app, "屏幕保修多久", &[&sales]).await;
    assert_eq!((found["semantic"].clone(), found["note"].clone()), (json!(true), Value::Null));
    assert_eq!(found["hits"][0]["document"], "保修条款.pdf");

    // 4. 智能体按知识库回答：带出处，出处随回答保存
    let agent = req(Method::POST, "/api/agents")
        .json(json!({ "name": "售后答疑", "folders": [sales] }))
        .send(&app)
        .await
        .json();
    let aid = agent["id"].as_str().unwrap();
    let cid = req(Method::POST, "/api/conversations").json(json!({ "agent_id": aid })).send(&app).await.json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/conversations/{cid}/messages");
    let r =
        req(Method::POST, &path).json(json!({ "content": "屏幕坏了还能保修吗", "provider_id": pid })).send(&app).await;
    let events = r.events();
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(&names[..2], ["user", "sources"]);
    let sources = &events[1].1["sources"];
    assert_eq!(sources[0]["n"], 1);
    assert_eq!(sources[0]["document"], "保修条款.pdf");
    let saved = &events.last().unwrap().1["message"];
    assert!(
        saved["content"].as_str().unwrap().contains("依据资料：") && saved["content"].as_str().unwrap().contains("[1]"),
        "{saved}"
    );
    assert_eq!(saved["sources"], *sources);
    let reread = req(Method::GET, &format!("/api/conversations/{cid}")).send(&app).await.json();
    assert_eq!(reread["messages"][1]["sources"][0]["document"], "保修条款.pdf");

    // 5. 这次对话临时改范围：只查招标文件；范围记在对话上
    let r = req(Method::POST, &path)
        .json(json!({ "content": "技术分和商务分各占多少", "provider_id": pid, "scope": [bids] }))
        .send(&app)
        .await;
    let src = r.events()[1].1["sources"].clone();
    assert!(src.as_array().unwrap().iter().all(|s| s["folder"].as_str().unwrap().starts_with("招标文件")), "{src}");
    assert_eq!(
        req(Method::GET, &format!("/api/conversations/{cid}")).send(&app).await.json()["conversation"]["scope"],
        json!([bids])
    );
    // 空范围：不查知识库，没有出处事件
    let r = req(Method::POST, &path)
        .json(json!({ "content": "随便聊聊", "provider_id": pid, "scope": [] }))
        .send(&app)
        .await;
    assert!(!r.events().iter().any(|(n, _)| n == "sources"));
    // 范围里什么都搜不到：告诉模型没有依据
    let empty = folder(&app, "空文件夹", None).await;
    let r = req(Method::POST, &path)
        .json(json!({ "content": "屏幕保修", "provider_id": pid, "scope": [empty] }))
        .send(&app)
        .await;
    let events = r.events();
    assert_eq!(events[1].1["sources"], json!([]));
    assert!(events.last().unwrap().1["message"]["content"].as_str().unwrap().contains("知识库里没有找到相关内容"));

    // 6. 查看片段、暂停与重试、移动、删除
    let doc_id = sales_docs["保修条款.pdf"]["id"].as_str().unwrap();
    let chunks = req(Method::GET, &format!("/api/kb/documents/{doc_id}/chunks")).send(&app).await.json();
    assert!(chunks["chunks"][0]["text"].as_str().unwrap().contains("保修条款"));
    let bad = bid_docs["坏文件.pdf"]["id"].as_str().unwrap();
    assert_eq!(
        req(Method::POST, &format!("/api/kb/documents/{bad}/retry")).send(&app).await.json()["status"],
        "queued"
    );
    assert_eq!(settle(&app, &bids).await["坏文件.pdf"]["status"], "failed");
    assert_eq!(
        req(Method::POST, &format!("/api/kb/documents/{bad}/explode")).send(&app).await.status,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        req(Method::PATCH, &format!("/api/kb/documents/{doc_id}"))
            .json(json!({ "folder_id": bids }))
            .send(&app)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(search(&app, "屏幕保修多久", &[&bids]).await["hits"][0]["document"], "保修条款.pdf");
    assert!(search(&app, "屏幕保修多久", &[&sales]).await["hits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|h| h["document"] != "保修条款.pdf"));

    assert_eq!(
        req(Method::DELETE, &format!("/api/kb/documents/{doc_id}")).send(&app).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(search(&app, "屏幕属于主要部件", &[&bids]).await["hits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|h| h["document"] != "保修条款.pdf"));

    // 文件夹：改名、不能移进自己的子文件夹、删除后内容一并消失
    assert_eq!(
        req(Method::PATCH, &format!("/api/kb/folders/{bids}"))
            .json(json!({ "move": true, "parent_id": bids_2026 }))
            .send(&app)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        req(Method::PATCH, &format!("/api/kb/folders/{bids_2026}"))
            .json(json!({ "name": "政务云二期" }))
            .send(&app)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(search(&app, "技术分", &[&bids]).await["hits"][0]["folder"], "招标文件 / 政务云二期");
    assert_eq!(req(Method::DELETE, &format!("/api/kb/folders/{bids}")).send(&app).await.status, StatusCode::NO_CONTENT);
    assert_eq!(search(&app, "技术分", &[&bids, &bids_2026]).await["hits"], json!([]));
    let listing = req(Method::GET, "/api/kb/folders").send(&app).await.json();
    assert_eq!(listing["folders"].as_array().unwrap().len(), 2);
    assert_eq!(listing["stats"]["documents"], 2);
}

#[tokio::test]
async fn retrieval_falls_back_to_keywords_when_embedding_service_is_down() {
    let app = setup().await;
    let base = commonbot_mock_llm::spawn(MockConfig::default()).await;
    let pid = add_provider(&app, &base, None).await;
    let f = folder(&app, "资料", None).await;
    req(Method::PUT, "/api/kb/settings")
        .json(json!({ "embed_provider_id": pid, "embed_model": "mock-embed" }))
        .send(&app)
        .await;
    upload(&app, &f, &[("延保说明.txt", fixture("gbk.txt"))]).await;
    assert_eq!(settle(&app, &f).await["延保说明.txt"]["embed_model"], "mock-embed");

    // 向量化模型换成一个不存在的：重新向量化失败，文档标为失败并给出原因
    req(Method::PUT, "/api/kb/settings")
        .json(json!({ "embed_provider_id": pid, "embed_model": "missing" }))
        .send(&app)
        .await;
    let d = settle(&app, &f).await["延保说明.txt"].clone();
    assert_eq!((d["status"].as_str().unwrap(), d["error_code"].as_str().unwrap()), ("failed", "model_error"));

    // 换回可用的模型后手动重试即可恢复
    req(Method::PUT, "/api/kb/settings")
        .json(json!({ "embed_provider_id": pid, "embed_model": "mock-embed" }))
        .send(&app)
        .await;
    req(Method::POST, &format!("/api/kb/documents/{}/retry", d["id"].as_str().unwrap())).send(&app).await;
    assert_eq!(settle(&app, &f).await["延保说明.txt"]["status"], "ready");

    // 提问时向量化服务出错：退回关键词，问答不中断
    let one = folder(&app, "另一份", None).await;
    upload(&app, &one, &[("a.txt", "保修期内免费维修。".as_bytes().to_vec())]).await;
    settle(&app, &one).await;
    req(Method::DELETE, &format!("/api/providers/{pid}")).send(&app).await;
    let found = search(&app, "免费维修", &[&one]).await;
    assert_eq!(found["semantic"], false);
    assert_eq!(found["hits"][0]["document"], "a.txt");
}
