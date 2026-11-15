//! 升级检查：从发行地址取签名的版本策略，验签后记下来。安装仍由人来做（下载新包、替换程序）。

use std::sync::Arc;
use std::time::Duration;

use commonbot_core::{store, update, Db};
use futures::StreamExt;

/// 版本策略的默认来源：GitHub 上最新发行版的附件。
const DEFAULT_BASE: &str = "https://github.com/golivegomedia2026-eng/CommonBot/releases/latest/download";
/// 策略文件很小；超过这个大小说明拿到的不是它。
const MAX_BYTES: usize = 64 * 1024;

pub struct Updater {
    base: String,
    /// 升级签名的公钥。为空表示这个构建不带升级检查。
    pubkey: String,
    http: reqwest::Client,
}

impl Updater {
    pub fn new(base: &str, pubkey: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), pubkey: pubkey.trim().to_string(), http }
    }

    /// 用编译时带进来的公钥。调试构建允许用环境变量指定公钥，便于本地演练升级流程。
    pub fn from_build() -> Self {
        let mut pubkey = env!("COMMONBOT_UPDATE_PUBKEY").to_string();
        if cfg!(debug_assertions) {
            if let Ok(k) = std::env::var("COMMONBOT_UPDATE_PUBKEY") {
                pubkey = k;
            }
        }
        Self::new(DEFAULT_BASE, &pubkey)
    }

    pub fn with_base(&self, base: &str) -> Self {
        Self::new(base, &self.pubkey)
    }

    pub fn enabled(&self) -> bool {
        !self.pubkey.is_empty()
    }

    async fn fetch(&self, name: &str) -> Option<Vec<u8>> {
        let resp = self.http.get(format!("{}/{name}", self.base)).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(part) = stream.next().await {
            out.extend_from_slice(&part.ok()?);
            if out.len() > MAX_BYTES {
                return None;
            }
        }
        Some(out)
    }

    /// 查一次。只有拿到验签通过的策略才算查成并记下来；其余情况一律不改变已有状态。
    pub async fn check(&self, db: &Db) -> bool {
        if !self.enabled() {
            return false;
        }
        let (Some(policy), Some(sig)) = (self.fetch("policy.json").await, self.fetch("policy.json.sig").await) else {
            return false;
        };
        let Some(verified) = update::verify(&policy, &String::from_utf8_lossy(&sig), &self.pubkey) else {
            return false;
        };
        db.run(move |c| update::remember(c, verified)).await.is_ok()
    }

    /// 启动后不久查一次，之后每 6 小时查一次。用户关掉了自动检查时不查。
    pub fn spawn(self: Arc<Self>, db: Db) {
        if !self.enabled() {
            return;
        }
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            loop {
                let auto =
                    db.run(|c| store::get_setting(c, "update.auto")).await.ok().flatten().as_deref() != Some("0");
                if auto {
                    self.check(&db).await;
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
    }
}
