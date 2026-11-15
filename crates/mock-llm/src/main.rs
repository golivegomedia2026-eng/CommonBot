//! 用法：`cargo run -p commonbot-mock-llm -- [端口]`，默认 11434（与 Ollama 相同，便于开箱演示）。

use std::time::Duration;

use commonbot_mock_llm::{router, MockConfig};

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(11434);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
    println!("mock model service on http://127.0.0.1:{port}/v1");
    let cfg = MockConfig { api_key: None, delay: Duration::from_millis(40) };
    axum::serve(listener, router(cfg)).await.expect("serve");
}
