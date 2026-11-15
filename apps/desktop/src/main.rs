//! CommonBot 桌面外壳：在进程里直接运行服务，开一个窗口显示它的界面，托盘常驻。
//! 关窗口只是收起来（局域网里的其他设备还在用服务）；从托盘菜单退出才真正结束。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use commonbot_server::run::{default_data_dir, run, Options};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const PORT: u16 = 8600;
/// 界面里要用系统浏览器打开的链接，先导航到这个不存在的地址，由外壳截住。
const OPEN_HOST: &str = "commonbot.invalid";

/// 托盘菜单的文字，按系统语言。
fn labels() -> (&'static str, &'static str) {
    let lang = std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).unwrap_or_default().to_lowercase();
    if lang.starts_with("zh_tw") || lang.starts_with("zh_hk") {
        ("開啟 CommonBot", "結束")
    } else if lang.starts_with("zh") {
        ("打开 CommonBot", "退出")
    } else if lang.starts_with("ja") {
        ("CommonBot を開く", "終了")
    } else {
        ("Open CommonBot", "Quit")
    }
}

fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 打开主窗口，显示 `url`（服务的界面，或启动失败时的说明页）。
fn open_window(app: &AppHandle, url: WebviewUrl, local_port: Option<u16>) {
    if let Some(w) = app.get_webview_window("main") {
        if let WebviewUrl::External(u) = &url {
            let _ = w.navigate(u.clone());
        }
        return show(app);
    }
    let built = WebviewWindowBuilder::new(app, "main", url)
        .title("CommonBot")
        .inner_size(1280.0, 860.0)
        .min_inner_size(380.0, 560.0)
        // 让网页自己处理拖放（上传文件要用）
        .disable_drag_drop_handler()
        // 带 target=_blank 的链接（网页出处、下载页、已发布的链接）交给系统浏览器
        .initialization_script(&format!(
            "document.addEventListener('click', (e) => {{ const a = e.target.closest && e.target.closest('a[target=\"_blank\"]'); if (a && a.href) {{ e.preventDefault(); location.href = 'http://{OPEN_HOST}/?u=' + encodeURIComponent(a.href); }} }}, true);"
        ))
        .on_navigation(move |url| {
            let host = url.host_str().unwrap_or("");
            if host == OPEN_HOST {
                if let Some((_, target)) = url.query_pairs().find(|(k, _)| k == "u") {
                    // 只交给系统打开网页链接，别的协议一律不理
                    if target.starts_with("http://") || target.starts_with("https://") {
                        let _ = open::that(target.as_ref());
                    }
                }
                return false;
            }
            // 窗口里只显示本机服务的界面和外壳自带的页面
            let ours = matches!(host, "localhost" | "127.0.0.1") && (local_port.is_none() || url.port() == local_port);
            ours || url.scheme() == "tauri" || host == "tauri.localhost"
        })
        .build();
    if let Err(e) = built {
        eprintln!("cannot open the window: {e}");
    }
}

/// 8600 端口上是不是已经有一个 CommonBot 在运行（比如装成了系统服务）。有的话直接用它，不再启动第二个。
async fn already_running() -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build() else {
        return false;
    };
    match client.get(format!("http://127.0.0.1:{PORT}/api/health")).send().await {
        Ok(r) => r.json::<serde_json::Value>().await.map(|v| v["name"] == "CommonBot").unwrap_or(false),
        Err(_) => false,
    }
}

fn main() {
    let has_tray = Arc::new(AtomicBool::new(false));
    let tray_flag = has_tray.clone();
    tauri::Builder::default()
        // 再次启动时不开第二份，把已有的窗口拿到前面
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .setup(move |app| {
            let handle = app.handle().clone();

            // 托盘：有些 Linux 桌面没有托盘，建不出来时关窗口就直接退出
            let (open_label, quit_label) = labels();
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "open", open_label, true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", quit_label, true, None::<&str>)?,
                ],
            )?;
            let mut tray = TrayIconBuilder::new().menu(&menu).tooltip("CommonBot").on_menu_event(|app, event| {
                match event.id.as_ref() {
                    "open" => show(app),
                    "quit" => app.exit(0),
                    _ => {}
                }
            });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray_flag.store(tray.build(app).is_ok(), Ordering::Relaxed);

            let resources = app.path().resource_dir().ok();
            tauri::async_runtime::spawn(async move {
                if already_running().await {
                    let url = format!("http://localhost:{PORT}").parse().expect("valid url");
                    return open_window(&handle, WebviewUrl::External(url), Some(PORT));
                }
                let Some(data_dir) = default_data_dir() else {
                    return open_window(
                        &handle,
                        WebviewUrl::App("index.html?error=cannot%20determine%20the%20data%20directory".into()),
                        None,
                    );
                };
                let opts = Options {
                    listen: None,
                    port: PORT,
                    data_dir,
                    // 安装包把 PDF 解析库放在资源目录里
                    pdfium_dirs: resources.into_iter().collect(),
                    // 8600 被别的程序占了就往后找一个空的
                    port_attempts: 10,
                };
                let opened = AtomicBool::new(false);
                let h = handle.clone();
                let on_listening = move |addr: SocketAddr, _code: Option<String>| {
                    // 开关局域网访问会重新监听，窗口只在第一次打开
                    if !opened.swap(true, Ordering::Relaxed) {
                        let url = format!("http://localhost:{}", addr.port()).parse().expect("valid url");
                        open_window(&h, WebviewUrl::External(url), Some(addr.port()));
                    }
                };
                if let Err(e) = run(opts, on_listening, std::future::pending()).await {
                    let page = format!("index.html?error={}", urlencode(&e.to_string()));
                    open_window(&handle, WebviewUrl::App(page.into()), None);
                }
            });
            // 服务起来之前先显示「正在启动」
            open_window(app.handle(), WebviewUrl::App("index.html".into()), None);
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if has_tray.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running CommonBot");
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}
