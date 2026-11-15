//! 谁能用接口。
//!
//! - 本机（回环地址连接，且浏览器地址栏里也是本机地址）：直接可用。
//! - 其他设备：先用访问码换取凭据（Cookie），之后凭它访问。
//! - 写操作若带 `Origin`，必须与访问的主机一致，防止别的网页借浏览器改这里的数据。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::{ApiError, AppState};

pub const COOKIE: &str = "cb_access";

/// 这次请求来自谁，由 [`guard`] 放进请求里。
#[derive(Clone, Copy, Debug)]
pub struct Caller {
    /// 在运行服务的这台机器上访问。
    pub local: bool,
    /// 本机，或已输入过正确的访问码。
    pub authed: bool,
}

/// 访问码连续输错的记录。
#[derive(Default)]
pub struct Failures {
    count: u32,
    locked_until: Option<Instant>,
}

const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

impl Failures {
    /// 还要等多少秒才能再试；`None` 表示可以试。
    pub fn wait_secs(&self) -> Option<u64> {
        let left = self.locked_until?.checked_duration_since(Instant::now())?;
        Some(left.as_secs().max(1))
    }
    pub fn record(&mut self, ok: bool) {
        if ok {
            *self = Self::default();
            return;
        }
        self.count += 1;
        if self.count >= MAX_FAILURES {
            self.count = 0;
            self.locked_until = Some(Instant::now() + LOCKOUT);
        }
    }
}

fn host_name(authority: &str) -> &str {
    // 去掉端口；IPv6 形如 [::1]:8600
    match authority.rfind(':') {
        Some(i) if !authority.ends_with(']') => &authority[..i],
        _ => authority,
    }
}

fn is_loopback_host(authority: &str) -> bool {
    matches!(host_name(authority).to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn header_str<'a>(headers: &'a HeaderMap, name: header::HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    header_str(headers, header::COOKIE)?
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// 比较时间不随内容变化，避免通过响应快慢猜出凭据。
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn guard(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host = header_str(headers, header::HOST).unwrap_or("");
    let peer_local = req.extensions().get::<ConnectInfo<SocketAddr>>().is_some_and(|c| c.0.ip().is_loopback());
    // 连接来自本机但 Host 不是本机地址（例如域名被指到 127.0.0.1）时，不当作本机对待。
    let local = peer_local && is_loopback_host(host);

    let safe_method = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe_method {
        if let Some(origin) = header_str(headers, header::ORIGIN) {
            let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
            if !origin_host.eq_ignore_ascii_case(host) {
                return ApiError::new(StatusCode::FORBIDDEN, "bad_origin", "cross-site request refused")
                    .into_response();
            }
        }
    }

    let authed = local || {
        let token = st.access_token.read().unwrap_or_else(|p| p.into_inner());
        let bearer = header_str(headers, header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer "));
        cookie(headers, COOKIE).or(bearer).is_some_and(|t| same(t, &token))
    };

    let path = req.uri().path();
    let open = !path.starts_with("/api/") || path == "/api/health" || path == "/api/access";
    if !open && !authed {
        return ApiError::new(StatusCode::UNAUTHORIZED, "access_required", "access code required").into_response();
    }
    req.extensions_mut().insert(Caller { local, authed });
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_loopback_hosts() {
        for h in ["localhost", "localhost:8600", "127.0.0.1:8600", "[::1]:8600", "[::1]", "LOCALHOST:1"] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in ["", "spark.local:8600", "192.168.1.5:8600", "localhost.evil.com", "evil.com:127"] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn locks_after_repeated_failures_and_resets_on_success() {
        let mut f = Failures::default();
        for _ in 0..MAX_FAILURES - 1 {
            f.record(false);
        }
        assert!(f.wait_secs().is_none());
        f.record(false);
        assert!(f.wait_secs().is_some());
        f.record(true);
        assert!(f.wait_secs().is_none());
    }

    #[test]
    fn reads_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; cb_access=tok; b=2".parse().unwrap());
        assert_eq!(cookie(&h, COOKIE), Some("tok"));
        assert_eq!(cookie(&h, "missing"), None);
        assert!(same("tok", "tok") && !same("tok", "toK") && !same("tok", "toke"));
    }
}
