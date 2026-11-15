//! 推理后端访问。只用 OpenAI 兼容接口（`/models`、`/chat/completions`），
//! Ollama、vLLM、llama.cpp 和多数云端模型都提供，所以后端可以随意更换。

use std::collections::VecDeque;
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// 形如 `http://127.0.0.1:11434/v1`，不带结尾的斜杠。
    pub base_url: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 流式回答里的一小段。
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// 正文。
    Content(String),
    /// 思考过程（部分模型会单独给出）。
    Reasoning(String),
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
}

impl Default for LlmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// 出错时最多保留多少字节的后端返回内容。
const ERROR_BODY_LIMIT: usize = 2048;
/// 一行流式数据的上限；超过说明对方不是在按协议返回。
const LINE_LIMIT: usize = 1024 * 1024;

impl LlmClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // 大模型出第一个字可能要等加载，读超时放宽；整体不设上限，由调用方取消。
            .read_timeout(Duration::from_secs(300))
            .build()
            .expect("http client");
        Self { http }
    }

    fn request(&self, method: reqwest::Method, ep: &Endpoint, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", ep.base_url.trim_end_matches('/'), path);
        let req = self.http.request(method, url);
        match ep.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    /// 列出后端上可用的模型，同时用作连通检测。
    pub async fn list_models(&self, ep: &Endpoint) -> Result<Vec<String>> {
        let resp = self
            .request(reqwest::Method::GET, ep, "/models")
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(unreachable)?;
        let resp = check_status(resp).await?;
        let body: Value = resp.json().await.map_err(|e| Error::Upstream { status: 200, message: e.to_string() })?;
        let mut models: Vec<String> = body["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        models.sort();
        Ok(models)
    }

    /// 发起一次流式对话。丢弃返回的流即取消请求。
    pub async fn chat_stream(
        &self,
        ep: &Endpoint,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<impl Stream<Item = Result<Delta>> + Send + Unpin> {
        let body = serde_json::json!({ "model": model, "messages": messages, "stream": true });
        let resp = self
            .request(reqwest::Method::POST, ep, "/chat/completions")
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        let resp = check_status(resp).await?;

        struct State<S> {
            bytes: S,
            parser: SseParser,
            ready: VecDeque<Result<Delta>>,
            finished: bool,
        }
        let state =
            State { bytes: resp.bytes_stream(), parser: SseParser::default(), ready: VecDeque::new(), finished: false };
        Ok(Box::pin(futures::stream::unfold(state, |mut st| async move {
            loop {
                if let Some(item) = st.ready.pop_front() {
                    return Some((item, st));
                }
                if st.finished {
                    return None;
                }
                match st.bytes.next().await {
                    Some(Ok(chunk)) => match st.parser.feed(&chunk) {
                        Ok(done) => {
                            st.ready.extend(st.parser.take().into_iter().map(Ok));
                            st.finished = done;
                        }
                        Err(e) => {
                            st.ready.push_back(Err(e));
                            st.finished = true;
                        }
                    },
                    Some(Err(e)) => {
                        st.ready.push_back(Err(Error::Unreachable(e.to_string())));
                        st.finished = true;
                    }
                    None => st.finished = true,
                }
            }
        })))
    }
}

/// 去掉部分模型写在正文里的思考段落。
fn strip_think(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

impl LlmClient {
    /// 把一批文字变成向量。返回的向量已归一化，可以直接用点积比较。
    pub async fn embed(&self, ep: &Endpoint, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({ "model": model, "input": inputs });
        let resp = self
            .request(reqwest::Method::POST, ep, "/embeddings")
            .timeout(Duration::from_secs(180))
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        let resp = check_status(resp).await?;
        let body: Value = resp.json().await.map_err(|e| Error::Upstream { status: 200, message: e.to_string() })?;
        let mut items: Vec<(u64, Vec<f32>)> = body["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let v = d["embedding"]
                            .as_array()
                            .map(|e| e.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                            .unwrap_or_default();
                        (d["index"].as_u64().unwrap_or(i as u64), v)
                    })
                    .collect()
            })
            .unwrap_or_default();
        items.sort_by_key(|(i, _)| *i);
        let dim = items.first().map(|(_, v)| v.len()).unwrap_or(0);
        if items.len() != inputs.len() || dim == 0 || items.iter().any(|(_, v)| v.len() != dim) {
            return Err(Error::Upstream { status: 200, message: "unexpected embeddings response".into() });
        }
        Ok(items.into_iter().map(|(_, v)| normalize(v)).collect())
    }

    /// 让看图模型把图片里的文字转写出来（用于扫描件）。
    pub async fn ocr_image(&self, ep: &Endpoint, model: &str, png: &[u8]) -> Result<String> {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        let body = serde_json::json!({
            "model": model,
            "stream": false,
            "temperature": 0,
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": OCR_PROMPT },
                { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{data}") } },
            ]}],
        });
        let resp = self
            .request(reqwest::Method::POST, ep, "/chat/completions")
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await
            .map_err(unreachable)?;
        let resp = check_status(resp).await?;
        let body: Value = resp.json().await.map_err(|e| Error::Upstream { status: 200, message: e.to_string() })?;
        let text = body["choices"][0]["message"]["content"].as_str().unwrap_or("");
        Ok(strip_think(text))
    }
}

const OCR_PROMPT: &str = "把这张图片里的全部文字按阅读顺序原样转写出来。表格按行输出，单元格之间用 | 分隔。只输出图片里的文字本身，不要解释、不要总结、不要补充图片里没有的内容。图片里没有文字时输出空。";

pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

fn unreachable(e: reqwest::Error) -> Error {
    Error::Unreachable(e.to_string())
}

async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::Unauthorized);
    }
    // 只读有限的一段，后端返回很大的错误页也不会占满内存。
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        body.extend_from_slice(&chunk);
        if body.len() >= ERROR_BODY_LIMIT {
            body.truncate(ERROR_BODY_LIMIT);
            break;
        }
    }
    let text = String::from_utf8_lossy(&body);
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(str::to_string))
        .unwrap_or_else(|| text.trim().to_string());
    Err(Error::Upstream { status: status.as_u16(), message })
}

/// 把后端返回的字节流拆成一段段回答。数据块可能在任意位置断开（包括一个汉字中间），
/// 所以先按字节攒够一整行再解码。
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    out: Vec<Delta>,
}

impl SseParser {
    /// 喂入一块数据。返回 `true` 表示后端已发出结束标记。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<bool> {
        self.buf.extend_from_slice(chunk);
        let mut done = false;
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if self.line(line.trim_end_matches(['\n', '\r']))? {
                done = true;
            }
        }
        if self.buf.len() > LINE_LIMIT {
            return Err(Error::Upstream { status: 200, message: "stream line too long".into() });
        }
        Ok(done)
    }

    pub fn take(&mut self) -> Vec<Delta> {
        std::mem::take(&mut self.out)
    }

    fn line(&mut self, line: &str) -> Result<bool> {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(false); // 空行、注释、event: 等
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        if let Some(err) = v.get("error") {
            let message = err["message"].as_str().or(err.as_str()).unwrap_or("unknown error").to_string();
            return Err(Error::Upstream { status: 200, message });
        }
        let delta = &v["choices"][0]["delta"];
        // 思考过程的字段名各家不同：reasoning_content（vLLM、DeepSeek）、reasoning（Ollama）。
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = delta[key].as_str().filter(|t| !t.is_empty()) {
                self.out.push(Delta::Reasoning(t.to_string()));
                break;
            }
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            self.out.push(Delta::Content(t.to_string()));
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str) -> String {
        format!("data: {}\n\n", serde_json::json!({ "choices": [{ "delta": { "content": text } }] }))
    }

    #[test]
    fn parses_content_split_at_any_byte() {
        let stream = format!(": keep-alive\n\n{}{}data: [DONE]\n\n", chunk("你好，"), chunk("世界"));
        let bytes = stream.as_bytes();
        // 在每一个字节位置断开都应得到同样的结果，包括汉字中间。
        for cut in 0..=bytes.len() {
            let mut p = SseParser::default();
            let d1 = p.feed(&bytes[..cut]).unwrap();
            let d2 = p.feed(&bytes[cut..]).unwrap();
            assert!(d1 || d2, "cut {cut}");
            assert_eq!(p.take(), vec![Delta::Content("你好，".into()), Delta::Content("世界".into())], "cut {cut}");
        }
    }

    #[test]
    fn separates_reasoning_and_handles_crlf() {
        let mut p = SseParser::default();
        p.feed(b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\r\n\r\n").unwrap();
        p.feed(b"data: {\"choices\":[{\"delta\":{\"reasoning\":\"more\",\"content\":\"\"}}]}\n").unwrap();
        p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n").unwrap();
        p.feed(b"data: {\"choices\":[]}\ndata: not json\n").unwrap();
        assert_eq!(
            p.take(),
            vec![Delta::Reasoning("think".into()), Delta::Reasoning("more".into()), Delta::Content("hi".into())]
        );
    }

    #[test]
    fn reports_error_sent_inside_the_stream() {
        let mut p = SseParser::default();
        let err = p.feed(b"data: {\"error\":{\"message\":\"out of memory\"}}\n").unwrap_err();
        assert!(matches!(err, Error::Upstream { message, .. } if message == "out of memory"));
    }

    #[test]
    fn strips_inline_thinking_and_normalizes() {
        assert_eq!(strip_think("<think>嗯</think>\n正文<think>再想</think>结尾"), "正文结尾");
        assert_eq!(strip_think("<think>没写完"), "");
        let v = normalize(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn rejects_endless_line() {
        let mut p = SseParser::default();
        assert!(p.feed(&vec![b'x'; LINE_LIMIT + 1]).is_err());
    }
}
