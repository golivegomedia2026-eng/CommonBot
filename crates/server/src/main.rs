use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use commonbot_core::{store, Db};
use commonbot_server::run::{banner, default_data_dir, run, Options};

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
        None => default_data_dir().ok_or("cannot determine the data directory; pass --data-dir")?,
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

    println!("{}", banner());
    println!("  data:  {}", data_dir.display());
    let opts = Options { listen: args.listen, port: args.port, data_dir, pdfium_dirs: Vec::new(), port_attempts: 0 };
    run(
        opts,
        |addr, code| {
            println!("  open:  http://localhost:{}", addr.port());
            if let Some(code) = code {
                println!("  other devices: http://<this machine's address>:{}  access code: {code}", addr.port());
            }
        },
        async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                if let Ok(mut term) = signal(SignalKind::terminate()) {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                    return;
                }
            }
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
    .map_err(|e| e.to_string().into())
}
