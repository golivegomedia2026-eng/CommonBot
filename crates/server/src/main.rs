use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use commonbot_core::kb::ingest::find_pdfium;
use commonbot_core::{store, Db};
use commonbot_server::{app, AppState, VERSION};

/// CommonBot 服务。
#[derive(Parser)]
#[command(name = "commonbot", version)]
struct Args {
    /// 固定监听这个地址。不指定时由界面里的「局域网访问」开关决定：关着只听本机，开着对局域网开放。
    #[arg(long, env = "COMMONBOT_LISTEN")]
    listen: Option<SocketAddr>,
    /// 端口（没有用 --listen 固定地址时生效）。
    #[arg(long, env = "COMMONBOT_PORT", default_value_t = 8600)]
    port: u16,
    /// 数据目录。默认在系统的应用数据目录下。
    #[arg(long, env = "COMMONBOT_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// 打开或关闭局域网访问（on / off），保存后退出。给没有显示器的机器用；正在运行的服务需要重启才生效。
    #[arg(long, value_name = "on|off")]
    lan: Option<String>,
    /// 打印其他设备连接时要输入的访问码，然后退出。
    #[arg(long)]
    access_code: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();

    let data_dir = match args.data_dir {
        Some(d) => d,
        None => dirs::data_dir().ok_or("cannot determine the data directory; pass --data-dir")?.join("CommonBot"),
    };
    let db = Db::open(&data_dir.join("commonbot.db"))?;
    // 两个一次性的命令：改设置或看访问码，做完就退出
    if let Some(v) = &args.lan {
        let on = match v.as_str() {
            "on" => true,
            "off" => false,
            _ => return Err("--lan takes on or off".into()),
        };
        db.run(move |c| store::set_setting(c, "lan", if on { "1" } else { "0" })).await?;
        println!("LAN access: {}. Restart CommonBot if it is running.", if on { "on" } else { "off" });
        if !on {
            return Ok(());
        }
    }
    if args.access_code || args.lan.is_some() {
        println!("access code: {}", db.run(|c| store::access_code(c)).await?);
        return Ok(());
    }

    // PDF 解析库：发行包里在程序旁边；从源码运行时在 vendor/ 下（scripts/fetch-vendor.sh）
    let vendor: Vec<PathBuf> = std::fs::read_dir("vendor/pdfium")
        .map(|d| d.flatten().flat_map(|e| [e.path().join("lib"), e.path().join("bin")]).collect())
        .unwrap_or_default();
    let pdfium = find_pdfium(&vendor.iter().map(PathBuf::as_path).collect::<Vec<_>>());

    let port = args.port;
    let wanted = |lan: bool| SocketAddr::from((if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] }, port));
    let lan_on = || {
        let db = db.clone();
        async move { db.run(|c| store::get_setting(c, "lan")).await.ok().flatten().as_deref() == Some("1") }
    };
    let first = match args.listen {
        Some(addr) => addr,
        None => wanted(lan_on().await),
    };
    let mut state = AppState::new(db.clone(), first, data_dir.clone(), pdfium.clone()).await?;
    state.listen_fixed = args.listen.is_some();
    if let Ok(base) = std::env::var("COMMONBOT_UPDATE_BASE") {
        // 只换下载来源；内容仍要用程序里的公钥验签
        state.updater = std::sync::Arc::new(state.updater.with_base(&base));
    }
    state.updater.clone().spawn(db.clone());
    let router = app(state.clone());

    println!("CommonBot {VERSION}");
    println!("  data:  {}", data_dir.display());
    if pdfium.is_none() {
        println!("  note:  PDF engine not found — PDF files cannot be added to the knowledge base");
    }

    // 主循环：按当前设置监听；界面里开关「局域网访问」后回到这里重新监听
    let mut addr = first;
    loop {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            // 对局域网开放失败（比如端口被别的网卡上的程序占了）时退回只听本机，服务不至于起不来
            Err(e) if !addr.ip().is_loopback() && args.listen.is_none() => {
                eprintln!("cannot listen on {addr}: {e}; falling back to this machine only");
                addr = wanted(false);
                continue;
            }
            Err(e) => return Err(format!("cannot listen on {addr}: {e}").into()),
        };
        let bound = listener.local_addr()?;
        *state.listen.write().unwrap_or_else(|p| p.into_inner()) = bound;
        println!("  open:  http://localhost:{}", bound.port());
        if !bound.ip().is_loopback() {
            let code = db.run(|c| store::access_code(c)).await?;
            println!("  other devices: http://<this machine's address>:{}  access code: {code}", bound.port());
        }

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let serve = axum::serve(listener, router.clone().into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .into_future();
        tokio::pin!(serve);
        let quit = tokio::select! {
            r = &mut serve => { r?; true }
            _ = tokio::signal::ctrl_c() => true,
            _ = state.rebind.notified() => false,
        };
        // 让进行中的请求收尾，但不无限等（流式回答、长连接可能一直不结束）
        let _ = stop_tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut serve).await;
        if quit {
            return Ok(());
        }
        addr = wanted(lan_on().await);
    }
}
