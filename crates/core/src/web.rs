//! 联网搜索：向搜索服务要结果，必要时读取网页正文，挑出与问题最相关的几段。
//! 网页内容只当资料用，绝不当指令执行；读取网页时拒绝指向本机和内网的地址。

use std::net::IpAddr;
use std::time::Duration;

use futures::StreamExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kb::chunk::tokens;
use crate::store::{get_setting, set_setting};
use crate::{Error, Result};

/// 搜索服务的设置。
#[derive(Debug, Clone, Serialize, Default)]
pub struct WebSettings {
    /// `builtin`（内置，直接向必应取结果，不需要任何配置）/ `none`（不联网）/ `searxng`（自建）/ `tavily` / `bocha`。
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
        // 没设置过时用内置搜索：不需要任何配置就能联网
        kind: if kind.is_empty() { "builtin".into() } else { kind },
        base_url: get("web.base_url")?,
        has_key: !api_key.is_empty(),
        api_key,
        fetch_pages: get("web.fetch_pages")? == "1",
    })
}

pub fn save_settings(c: &Connection, input: WebSettingsInput) -> Result<WebSettings> {
    let kind = input.kind.trim();
    if !matches!(kind, "builtin" | "none" | "searxng" | "tavily" | "bocha") {
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
    /// 取搜索结果用：允许跟随少量跳转（必应会按地区在 www 和 cn 之间跳转）。
    http_follow: reqwest::Client,
    /// 内置搜索向这个地址取结果。测试里指向假服务。
    pub bing_base: String,
    /// 允许读取本机和内网地址上的网页。只在测试里打开。
    pub allow_private: bool,
}

/// 向搜索引擎取结果时用的浏览器标识。
const BROWSER_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

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
        let http_follow = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent(BROWSER_UA)
            .build()
            .expect("http client");
        Self { http, http_follow, allow_private, bing_base: "https://www.bing.com".into() }
    }

    /// 搜索，返回最多 `limit` 条结果。
    pub async fn search(&self, s: &WebSettings, query: &str, limit: usize) -> Result<Vec<WebResult>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut results = match s.kind.as_str() {
            "builtin" => self.builtin(query).await?,
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

    /// 内置搜索：直接向必应取结果，不经过任何中间服务，也不需要密钥。
    /// 先取它的 RSS 输出（结构简单稳定）；取不到再解析网页版。
    /// 搜索引擎对程序发出的请求有时会返回不相干的内容，所以结果要过一道相关性检查。
    async fn builtin(&self, query: &str) -> Result<Vec<WebResult>> {
        let fetch = |extra: &'static str| {
            let url = format!("{}/search", self.bing_base.trim_end_matches('/'));
            let mut req = self.http_follow.get(url).query(&[("q", query)]);
            if !extra.is_empty() {
                req = req.query(&[("format", extra)]);
            }
            async move {
                let resp = req
                    .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .map_err(upstream)?;
                if !resp.status().is_success() {
                    return Err(upstream(format!("HTTP {}", resp.status().as_u16())));
                }
                let bytes = resp.bytes().await.map_err(upstream)?;
                Ok::<String, Error>(String::from_utf8_lossy(&bytes[..bytes.len().min(PAGE_LIMIT)]).into_owned())
            }
        };
        let mut results = match fetch("rss").await {
            Ok(xml) => parse_bing_rss(&xml),
            Err(_) => Vec::new(),
        };
        if results.is_empty() {
            results = parse_bing_html(&fetch("").await?);
        }
        let found = results.len();
        let relevant = keep_relevant(results, query);
        if found > 0 && relevant.is_empty() {
            // 搜到了东西但全都不相干：当作这次没搜成，比把无关内容交给模型好
            return Err(Error::Web("web_irrelevant", String::new()));
        }
        Ok(relevant)
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

/// 把 HTML/XML 里的转义字符还原。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // 按字节找分号：& 后面可能紧跟中文，按固定字节数截取字符串会切在半个字上
        let Some(end) = rest.as_bytes().iter().take(12).position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" | "ensp" | "emsp" => Some(' '),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 去掉标签，只留文字。
fn text_only(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)?;
    Some(&s[i..i + j])
}

/// 必应网页版里的结果链接有时是跳转地址（`/ck/a?...&u=a1<base64>`），还原成真正的网址。
fn bing_target(href: &str) -> String {
    use base64::Engine as _;
    let href = unescape(href);
    if !href.contains("bing.com/ck/a") {
        return href;
    }
    href.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("u=a1"))
        .and_then(|b64| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64.trim_end_matches('=')).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .unwrap_or_default()
}

/// 解析必应的 RSS 输出。
pub fn parse_bing_rss(xml: &str) -> Vec<WebResult> {
    xml.split("<item>")
        .skip(1)
        .filter_map(|item| {
            let item = item.split("</item>").next()?;
            let url = unescape(between(item, "<link>", "</link>")?.trim());
            (url.starts_with("http://") || url.starts_with("https://")).then(|| WebResult {
                title: text_only(between(item, "<title>", "</title>").unwrap_or("")),
                url,
                text: text_only(between(item, "<description>", "</description>").unwrap_or("")),
            })
        })
        .collect()
}

/// 解析必应的网页版结果页。
pub fn parse_bing_html(html: &str) -> Vec<WebResult> {
    html.split("class=\"b_algo\"")
        .skip(1)
        .filter_map(|block| {
            let block = block.split("</li>").next()?;
            let h2 = between(block, "<h2", "</h2>")?;
            let url = bing_target(between(h2, "href=\"", "\"")?);
            if url.is_empty() {
                return None;
            }
            let title = text_only(&h2[h2.find('>')? + 1..]);
            let text = block
                .find("b_caption")
                .and_then(|i| {
                    let caption = &block[i..];
                    let p = &caption[caption.find("<p")?..];
                    Some(text_only(&p[..p.find("</p>")?]))
                })
                .unwrap_or_default();
            Some(WebResult { title, url, text })
        })
        .collect()
}

/// 只留下与问题至少有一个词对得上的结果。
pub fn keep_relevant(results: Vec<WebResult>, query: &str) -> Vec<WebResult> {
    let wanted: std::collections::HashSet<String> = tokens(query).into_iter().collect();
    if wanted.is_empty() {
        return results;
    }
    results
        .into_iter()
        .filter(|r| tokens(&format!("{} {}", r.title, r.text)).iter().any(|t| wanted.contains(t)))
        .collect()
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

/// 从一长段文字里挑出与问题用词有重合的行，重合多的优先，总长不超过 `budget` 字，保持原文顺序。
/// 一行都对不上时取开头部分。按行挑是为了甩掉网页里的导航、页脚这类无关内容。
pub fn best_passages(text: &str, query: &str, budget: usize) -> String {
    let wanted: std::collections::HashSet<String> = tokens(query).into_iter().collect();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let mut scored: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let have: std::collections::HashSet<String> = tokens(line).into_iter().collect();
            (wanted.intersection(&have).count(), i)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    if scored.is_empty() {
        return lines.join("\n").chars().take(budget).collect();
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut picked: Vec<usize> = Vec::new();
    let mut used = 0;
    for (_, i) in scored {
        let len = lines[i].chars().count() + 1;
        if used + len > budget && !picked.is_empty() {
            continue;
        }
        picked.push(i);
        used += len;
    }
    picked.sort();
    // 不相邻的行之间加省略号，表示中间略去了内容
    let mut out = String::new();
    for (n, &i) in picked.iter().enumerate() {
        if n > 0 {
            out.push_str(if picked[n - 1] + 1 == i { "\n" } else { "\n…\n" });
        }
        out.push_str(lines[i]);
    }
    out.chars().take(budget).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
            .unwrap()
    }

    #[test]
    fn parses_real_bing_rss() {
        let r = parse_bing_rss(&sample("bing.rss.xml"));
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].url, "https://github.com/tokio-rs/axum");
        assert!(r[1].title.starts_with("GitHub - tokio-rs/axum"));
        assert!(r[1].text.contains("HTTP routing and request-handling library"));
        assert!(parse_bing_rss("<rss><channel></channel></rss>").is_empty());
        assert!(parse_bing_rss("not xml at all").is_empty());
    }

    #[test]
    fn parses_real_bing_page_and_resolves_redirect_links() {
        let r = parse_bing_html(&sample("bing.html"));
        assert_eq!(r.len(), 4, "{r:?}");
        // 跳转地址被还原成真正的网址
        assert_eq!(r[0].url, "https://en.wikipedia.org/wiki/Axum");
        assert_eq!(r[1].url, "https://github.com/tokio-rs/axum");
        assert!(r.iter().all(|x| !x.url.contains("bing.com/ck/")));
        // 直接给出网址的结果、中文标题里的强调标签和转义字符
        assert_eq!(r[3].url, "https://smart.mail.163.com/login.htm");
        assert_eq!(r[3].title, "网易163免费邮箱登录");
        assert!(
            r[3].text.contains("@163.com") && !r[3].text.contains('<') && !r[3].text.contains("&ensp;"),
            "{}",
            r[3].text
        );
        assert!(parse_bing_html("<html><body>captcha</body></html>").is_empty());
    }

    #[test]
    fn drops_results_that_share_no_word_with_the_question() {
        let r = |title: &str, text: &str| WebResult { title: title.into(), url: "https://x".into(), text: text.into() };
        let kept = keep_relevant(
            vec![r("家电延保新规", "延保价格调整"), r("洛丹伦瘟疫爆发之前", "是谁变成了巫妖王")],
            "家电延保政策",
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].title, "家电延保新规");
        assert_eq!(keep_relevant(vec![r("anything", "")], "，。").len(), 1, "nothing to compare against: keep");
    }

    #[test]
    fn unescapes_entities() {
        assert_eq!(
            unescape("A &amp; B &lt;c&gt; &#20013;&#x6587; &unknown; 100% & done"),
            "A & B <c> 中文 &unknown; 100% & done"
        );
        assert_eq!(text_only("<b>粗</b>体&nbsp;<i>字</i>\n  换行"), "粗体 字 换行");
        // & 后面紧跟多字节文字
        assert_eq!(unescape("研发&测试部门；A&B"), "研发&测试部门；A&B");
        assert_eq!(unescape("末尾&"), "末尾&");
    }

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
        assert!(!got.contains("网站导航"), "unrelated lines are left out: {got}");
        // 一行都对不上：取开头
        assert!(best_passages("第一行\n第二行", "xyz", 100).starts_with("第一行"));
        assert_eq!(best_passages("", "x", 100), "");
    }

    #[tokio::test]
    async fn settings_round_trip_and_keep_the_key_secret() {
        let db = crate::Db::open_in_memory().unwrap();
        db.run(|c| {
            assert_eq!(settings(c)?.kind, "builtin", "works out of the box with the built-in search");
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
