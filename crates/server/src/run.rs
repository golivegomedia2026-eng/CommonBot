//! 启动并运行服务。命令行程序和桌面外壳都用这一份。

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::path::PathBuf;

use commonbot_core::kb::ingest::find_pdfium;
use commonbot_core::{store, Db};

use crate::{app, AppState, VERSION};

pub struct Options {
    /// 固定监听这个地址。`None` 时由界面里的「局域网访问」开关决定。
    pub listen: Option<SocketAddr>,
    pub port: u16,
    pub data_dir: PathBuf,
    /// 除默认位置外，还到这些目录里找 PDF 解析库。
    pub pdfium_dirs: Vec<PathBuf>,
    /// 端口被占用时依次往后试几个端口（桌面外壳用；命令行程序不试，直接报错）。
    pub port_attempts: u16,
}

/// 默认的数据目录：系统的应用数据目录下的 CommonBot。
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("CommonBot"))
}

/// 运行服务，直到 `shutdown` 完成。每次开始监听（启动时、开关局域网访问后）都调用一次 `on_listening`。
pub async fn run(
    opts: Options,
    on_listening: impl Fn(SocketAddr, Option<String>) + Send + Sync,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let db = Db::open(&opts.data_dir.join("commonbot.db"))?;
    // PDF 解析库：发行包里在程序旁边；从源码运行时在 vendor/ 下（scripts/fetch-vendor.sh）
    let mut dirs: Vec<PathBuf> = opts.pdfium_dirs.clone();
    dirs.extend(
        std::fs::read_dir("vendor/pdfium")
            .map(|d| d.flatten().flat_map(|e| [e.path().join("lib"), e.path().join("bin")]).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    let pdfium = find_pdfium(&dirs.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    if pdfium.is_none() {
        eprintln!("note: PDF engine not found — PDF files cannot be added to the knowledge base");
    }

    let port = opts.port;
    let wanted = |lan: bool, port: u16| SocketAddr::from((if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] }, port));
    let lan_on = || {
        let db = db.clone();
        async move { db.run(|c| store::get_setting(c, "lan")).await.ok().flatten().as_deref() == Some("1") }
    };
    let first = match opts.listen {
        Some(addr) => addr,
        None => wanted(lan_on().await, port),
    };
    let mut state = AppState::new(db.clone(), first, opts.data_dir.clone(), pdfium).await?;
    state.listen_fixed = opts.listen.is_some();
    if let Ok(base) = std::env::var("COMMONBOT_UPDATE_BASE") {
        // 只换下载来源；内容仍要用程序里的公钥验签
        state.updater = std::sync::Arc::new(state.updater.with_base(&base));
    }
    state.updater.clone().spawn(db.clone());
    let router = app(state.clone());
    tokio::pin!(shutdown);

    // 主循环：按当前设置监听；界面里开关「局域网访问」后回到这里重新监听
    let mut addr = first;
    let mut attempts = 0u16;
    loop {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            // 端口被占：允许的话换下一个端口
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempts < opts.port_attempts => {
                attempts += 1;
                addr.set_port(addr.port() + 1);
                continue;
            }
            // 对局域网开放失败时退回只听本机，服务不至于起不来
            Err(e) if !addr.ip().is_loopback() && opts.listen.is_none() => {
                eprintln!("cannot listen on {addr}: {e}; falling back to this machine only");
                addr = wanted(false, addr.port());
                continue;
            }
            Err(e) => return Err(format!("cannot listen on {addr}: {e}").into()),
        };
        let bound = listener.local_addr()?;
        *state.listen.write().unwrap_or_else(|p| p.into_inner()) = bound;
        let code = if bound.ip().is_loopback() { None } else { Some(db.run(|c| store::access_code(c)).await?) };
        on_listening(bound, code);

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let serve = axum::serve(listener, router.clone().into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .into_future();
        tokio::pin!(serve);
        let quit = tokio::select! {
            r = &mut serve => { r?; true }
            _ = &mut shutdown => true,
            _ = state.rebind.notified() => false,
        };
        // 让进行中的请求收尾，但不无限等（流式回答、长连接可能一直不结束）
        let _ = stop_tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut serve).await;
        if quit {
            return Ok(());
        }
        addr = wanted(lan_on().await, bound.port());
    }
}

/// 启动时打印的版本行。
pub fn banner() -> String {
    format!("CommonBot {VERSION}")
}
