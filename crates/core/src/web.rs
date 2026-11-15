//! 联网搜索：向搜索服务要结果，必要时读取网页正文，挑出与问题最相关的几段。
//! 网页内容只当资料用，绝不当指令执行；读取网页时拒绝指向本机和内网的地址。

use std::net::IpAddr;
use std::time::Duration;

use futures::StreamExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kb::chunk::{chunk, tokens};
use crate::kb::parse::{Locator, Section};
use crate::store::{get_setting, set_setting};
use crate::{Error, Result};

/// 搜索服务的设置。
#[derive(Debug, Clone, Serialize, Default)]
pub struct WebSettings {
    /// `none`（不联网）/ `searxng`（自建，不需要密钥）/ `tavily` / `bocha`。
    pub kind: String,
    /// 自建搜索的地址，如 `http://127.0.0.1:8888`。
    pub base_url: String,
    #[serde(skip_serializing)]
    pub api_key: String,
    pub has_key: bool,
    /// 是否进一步读取网页正文（更准，但更慢）。
    pub fetch_pages: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct WebSettingsInput {
    pub kind: String,
    pub base_url: String,
    /// 不传：保持原样；空字符串：清除。
    pub api_key: Option<String>,
    pub fetch_pages: bool,
}

pub fn settings(c: &Connection) -> Result<WebSettings> {
    let get = |k: &str| get_setting(c, k).map(|v| v.unwrap_or_default());
    let api_key = get("web.api_key")?;
    let kind = get("web.kind")?;
    Ok(WebSettings {
        kind: if kind.is_empty() { "none".into() } else { kind },
        base_url: get("web.base_url")?,
        has_key: !api_key.is_empty(),
        api_key,
        fetch_pages: get("web.fetch_pages")? == "1",
    })
}

pub fn save_settings(c: &Connection, input: WebSettingsInput) -> Result<WebSettings> {
    let kind = input.kind.trim();
    if !matches!(kind, "none" | "searxng" | "tavily" | "bocha") {
        return Err(Error::Invalid("kind".into()));
    }
    let base_url = input.base_url.trim().trim_end_matches('/');
    if kind == "searxng" {
        match reqwest::Url::parse(base_url) {
            Ok(u) if (u.scheme() == "http" || u.scheme() == "https") && u.has_host() => {}
            _ => return Err(Error::Invalid("base_url".into())),
        }
    }
    set_setting(c, "web.kind", kind)?;
    set_setting(c, "web.base_url", base_url)?;
    set_setting(c, "web.fetch_pages", if input.fetch_pages { "1" } else { "0" })?;
    if let Some(key) = input.api_key {
        set_setting(c, "web.api_key", key.trim())?;
    }
    settings(c)
}

/// 一条搜索结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    /// 摘要，或从网页正文里挑出的相关段落。
    pub text: String,
}

#[derive(Clone)]
pub struct WebClient {
    http: reqwest::Client,
    /// 允许读取本机和内网地址上的网页。只在测试里打开。
    pub allow_private: bool,
}

/// 读取网页正文时的限制。
const PAGE_LIMIT: usize = 1_500_000;
const PAGE_TIMEOUT: Duration = Duration::from_secs(8);
/// 每个网页最多带给模型多少字。
const EXCERPT_CHARS: usize = 1200;

fn upstream(e: impl std::fmt::Display) -> Error {
    Error::Web("web_search_failed", e.to_string())
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

impl WebClient {
    pub fn new(allow_private: bool) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // 不自动跟随跳转：跳转目标可能是内网地址
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Mozilla/5.0 (compatible; CommonBot)")
            .build()
            .expect("http client");
        Self { http, allow_private }
    }

    /// 搜索，返回最多 `limit` 条结果。
    pub async fn search(&self, s: &WebSettings, query: &str, limit: usize) -> Result<Vec<WebResult>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut results = match s.kind.as_str() {
            "searxng" => self.searxng(s, query).await?,
            "tavily" => self.tavily(s, query, limit).await?,
            "bocha" => self.bocha(s, query, limit).await?,
            _ => return Err(Error::Web("web_not_configured", String::new())),
        };
        results.retain(|r| !r.url.is_empty());
        results.truncate(limit);
        if s.fetch_pages {
            // 同时读取各个网页；读不到的保留搜索服务给的摘要
            let pages = futures::future::join_all(results.iter().map(|r| self.page_excerpt(&r.url, query))).await;
            for (r, page) in results.iter_mut().zip(pages) {
                if let Some(text) = page {
                    r.text = text;
                }
            }
        }
        results.iter_mut().for_each(|r| r.text = r.text.chars().take(EXCERPT_CHARS).collect());
        Ok(results)
    }

    async fn json(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.timeout(Duration::from_secs(20)).send().await.map_err(upstream)?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Web("web_unauthorized", String::new()));
        }
        if !status.is_success() {
            return Err(upstream(format!("HTTP {}", status.as_u16())));
        }
        resp.json().await.map_err(upstream)
    }

    async fn searxng(&self, s: &WebSettings, query: &str) -> Result<Vec<WebResult>> {
        let url = format!("{}/search", s.base_url.trim_end_matches('/'));
        let body = self.json(self.http.get(url).query(&[("q", query), ("format", "json")])).await?;
        Ok(body["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| WebResult {
                        title: str_of(&r["title"]),
                        url: str_of(&r["url"]),
                        text: str_of(&r["content"]),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn tavily(&self, s: &WebSettings, query: &str, limit: usize) -> Result<Vec<WebResult>> {
        let req = self
            .http
            .post("https://api.tavily.com/search")
            .bearer_auth(&s.api_key)
            .json(&serde_json::json!({ "query": query, "max_results": limit }));
        let body = self.json(req).await?;
        Ok(body["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| WebResult {
                        title: str_of(&r["title"]),
                        url: str_of(&r["url"]),
                        text: str_of(&r["content"]),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn bocha(&self, s: &WebSettings, query: &str, limit: usize) -> Result<Vec<WebResult>> {
        let req = self
            .http
            .post("https://api.bochaai.com/v1/web-search")
            .bearer_auth(&s.api_key)
            .json(&serde_json::json!({ "query": query, "count": limit, "summary": true }));
        let body = self.json(req).await?;
        Ok(body["data"]["webPages"]["value"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        let summary = str_of(&r["summary"]);
                        WebResult {
                            title: str_of(&r["name"]),
                            url: str_of(&r["url"]),
                            text: if summary.is_empty() { str_of(&r["snippet"]) } else { summary },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 读取一个网页，返回其中与问题最相关的几段。读不到或不允许读时返回 `None`。
    async fn page_excerpt(&self, url: &str, query: &str) -> Option<String> {
        let parsed = reqwest::Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        if !self.allow_private && !is_public_host(&parsed).await {
            return None;
        }
        let resp = self.http.get(parsed).timeout(PAGE_TIMEOUT).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        if !(kind.contains("html") || kind.starts_with("text/plain")) {
            return None;
        }
        let mut bytes = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(Ok(part)) = stream.next().await {
            bytes.extend_from_slice(&part);
            if bytes.len() > PAGE_LIMIT {
                bytes.truncate(PAGE_LIMIT);
                break;
            }
        }
        let html = crate::kb::parse::decode(&bytes);
        let text = html2text::from_read(std::io::Cursor::new(html.into_bytes()), 200).ok()?;
        let picked = best_passages(&text, query, EXCERPT_CHARS);
        (!picked.trim().is_empty()).then_some(picked)
    }
}

/// 地址是否在公网上。域名解析出的每一个地址都要是公网地址才算。
async fn is_public_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    let port = url.port_or_known_default().unwrap_or(80);
    match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => {
            let addrs: Vec<_> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| is_public_ip(a.ip()))
        }
        Err(_) => false,
    }
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || o[0] == 0
                // 运营商级 NAT 100.64.0.0/10
                || (o[0] == 100 && (64..128).contains(&o[1])))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // 唯一本地地址 fc00::/7、链路本地 fe80::/10
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// 从一长段文字里挑出与问题用词重合最多的几段，总长不超过 `budget` 字。保持原文顺序。
pub fn best_passages(text: &str, query: &str, budget: usize) -> String {
    let wanted: std::collections::HashSet<String> = tokens(query).into_iter().collect();
    let pieces = chunk(&[Section { locator: Locator::None, text: text.to_string() }]);
    let mut scored: Vec<(usize, usize, usize)> = pieces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let have: std::collections::HashSet<String> = tokens(&p.text).into_iter().collect();
            (wanted.intersection(&have).count(), i, p.text.chars().count())
        })
        .collect();
    // 重合多的在前；一样多时靠前的段落在前（网页开头通常是正文要点）
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut picked: Vec<usize> = Vec::new();
    let mut used = 0;
    for (_, i, len) in scored {
        if used + len > budget && !picked.is_empty() {
            continue;
        }
        picked.push(i);
        used += len;
        if used >= budget {
            break;
        }
    }
    picked.sort();
    let joined = picked.iter().map(|&i| pieces[i].text.as_str()).collect::<Vec<_>>().join("\n…\n");
    joined.chars().take(budget).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_local_and_private_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.5",
            "172.16.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.1.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111", "100.128.0.1"] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn picks_passages_that_match_the_question() {
        let filler = "这一段讲的是与问题无关的网站导航和版权声明。".repeat(40);
        let text = format!("{filler}\n屏幕属于主要部件，保修期为二十四个月，自购买之日起计算。\n{filler}");
        let got = best_passages(&text, "屏幕保修多久", 700);
        assert!(got.contains("保修期为二十四个月"), "{got}");
        assert!(got.chars().count() <= 700);
        assert_eq!(best_passages("", "x", 100), "");
    }

    #[tokio::test]
    async fn settings_round_trip_and_keep_the_key_secret() {
        let db = crate::Db::open_in_memory().unwrap();
        db.run(|c| {
            assert_eq!(settings(c)?.kind, "none");
            let bad = WebSettingsInput { kind: "searxng".into(), base_url: "not a url".into(), ..Default::default() };
            assert!(matches!(save_settings(c, bad), Err(Error::Invalid(f)) if f == "base_url"));
            assert!(save_settings(c, WebSettingsInput { kind: "other".into(), ..Default::default() }).is_err());

            let s = save_settings(
                c,
                WebSettingsInput {
                    kind: "tavily".into(),
                    api_key: Some("tvly-secret".into()),
                    fetch_pages: true,
                    ..Default::default()
                },
            )?;
            assert!(s.has_key && s.fetch_pages);
            assert!(!serde_json::to_string(&s).unwrap().contains("tvly-secret"));
            // 不传密钥：保持原样
            let s = save_settings(c, WebSettingsInput { kind: "tavily".into(), ..Default::default() })?;
            assert_eq!(s.api_key, "tvly-secret");
            let s = save_settings(
                c,
                WebSettingsInput { kind: "none".into(), api_key: Some(String::new()), ..Default::default() },
            )?;
            assert!(!s.has_key);
            Ok(())
        })
        .await
        .unwrap();
    }
}
