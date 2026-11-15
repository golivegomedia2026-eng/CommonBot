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
    /// 监听地址。默认只有本机能访问；要让局域网里的其他设备访问，用 0.0.0.0:8600。
    #[arg(long, env = "COMMONBOT_LISTEN", default_value = "127.0.0.1:8600")]
    listen: SocketAddr,
    /// 数据目录。默认在系统的应用数据目录下。
    #[arg(long, env = "COMMONBOT_DATA_DIR")]
    data_dir: Option<PathBuf>,
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
    // PDF 解析库：发行包里在程序旁边；从源码运行时在 vendor/ 下（scripts/fetch-vendor.sh）
    let vendor: Vec<PathBuf> = std::fs::read_dir("vendor/pdfium")
        .map(|d| d.flatten().flat_map(|e| [e.path().join("lib"), e.path().join("bin")]).collect())
        .unwrap_or_default();
    let pdfium = find_pdfium(&vendor.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    let state = AppState::new(db.clone(), args.listen, data_dir.clone(), pdfium.clone()).await?;

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", args.listen))?;
    let addr = listener.local_addr()?;
    println!("CommonBot {VERSION}");
    println!("  data:  {}", data_dir.display());
    println!("  open:  http://localhost:{}", addr.port());
    if pdfium.is_none() {
        println!("  note:  PDF engine not found — PDF files cannot be added to the knowledge base");
    }
    if !addr.ip().is_loopback() {
        let code = db.run(|c| store::access_code(c)).await?;
        println!("  other devices: http://<this machine's address>:{}  access code: {code}", addr.port());
    }

    axum::serve(listener, app(state).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
