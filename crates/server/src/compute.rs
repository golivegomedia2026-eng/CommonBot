//! 本机算力：发现「算力包」（推理运行时 + 模型），启动并看护它，第一次用时把各个模型派好用场。
//! 算力包由 scripts/compute-pack.sh 生成，是一个 compute 文件夹：runtime/（Ollama）、models/、pack.json。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use commonbot_core::store::ProviderInput;
use commonbot_core::{kb, store, Db};
use serde::{Deserialize, Serialize};

use crate::compute_get::{Progress, Sources};

/// 算力包里的说明：各个模型派什么用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PackInfo {
    pub platform: String,
    pub built: String,
    pub chat: String,
    pub extra: Vec<String>,
    pub embed: String,
    pub vision: String,
}

#[derive(Debug, Clone)]
pub struct Pack {
    pub dir: PathBuf,
    pub binary: PathBuf,
    pub info: PackInfo,
}

/// 在这些位置里找算力包：每个位置本身，或它下面的 compute 文件夹。
pub fn find_pack(places: &[PathBuf]) -> Option<Pack> {
    let exe = if cfg!(windows) { "ollama.exe" } else { "ollama" };
    places.iter().flat_map(|p| [p.join("compute"), p.clone()]).find_map(|dir| {
        let binary =
            [dir.join("runtime/bin").join(exe), dir.join("runtime").join(exe)].into_iter().find(|b| b.is_file())?;
        let info = std::fs::read_to_string(dir.join("pack.json")).ok().and_then(|s| serde_json::from_str(&s).ok())?;
        Some(Pack { dir, binary, info })
    })
}

/// 默认去哪里找：环境变量指定的位置、程序旁边、数据目录。
pub fn default_places(data_dir: &Path) -> Vec<PathBuf> {
    let mut places = Vec::new();
    if let Ok(p) = std::env::var("COMMONBOT_COMPUTE_DIR") {
        places.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        places.push(dir);
    }
    places.push(data_dir.to_path_buf());
    places
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// 没有算力包。
    None,
    /// 端口上已经有推理服务在运行（比如用户自己装的 Ollama）：直接用它，不再启动随包的运行时。
    External,
    Starting,
    Running,
    /// 启动不了。`detail` 是原因。
    Failed {
        detail: String,
    },
}

pub struct Compute {
    pack: RwLock<Option<Pack>>,
    /// 数据目录：在界面里下载的算力包放在它下面的 compute 文件夹。
    pub(crate) data_dir: PathBuf,
    /// 下载运行时和模型的来源。
    pub(crate) sources: Sources,
    /// 在界面里下载算力包的进度。
    pub(crate) progress: Mutex<Progress>,
    pub(crate) cancel: AtomicBool,
    /// 推理服务监听的端口。
    pub port: u16,
    state: RwLock<State>,
    child: Mutex<Option<tokio::process::Child>>,
    log: PathBuf,
}

impl Compute {
    pub fn new(pack: Option<Pack>, port: u16, data_dir: &Path) -> Arc<Self> {
        Self::with_sources(pack, port, data_dir, Sources::from_env())
    }

    pub fn with_sources(pack: Option<Pack>, port: u16, data_dir: &Path, sources: Sources) -> Arc<Self> {
        Arc::new(Self {
            pack: RwLock::new(pack),
            data_dir: data_dir.to_path_buf(),
            sources,
            progress: Mutex::default(),
            cancel: AtomicBool::new(false),
            port,
            state: RwLock::new(State::None),
            child: Mutex::new(None),
            log: data_dir.join("logs/compute.log"),
        })
    }

    pub fn pack(&self) -> Option<Pack> {
        self.pack.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn set_pack(&self, pack: Pack) {
        *self.pack.write().unwrap_or_else(|p| p.into_inner()) = Some(pack);
    }

    pub fn state(&self) -> State {
        self.state.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, s: State) {
        *self.state.write().unwrap_or_else(|p| p.into_inner()) = s;
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub(crate) async fn answering(&self) -> bool {
        let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else { return false };
        client.get(format!("{}/models", self.base_url())).send().await.is_ok_and(|r| r.status().is_success())
    }

    fn launch(&self, pack: &Pack) -> std::io::Result<tokio::process::Child> {
        if let Some(dir) = self.log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        tokio::process::Command::new(&pack.binary)
            .arg("serve")
            // 只听本机；模型从算力包里读，不联网下载
            .env("OLLAMA_HOST", format!("127.0.0.1:{}", self.port))
            .env("OLLAMA_MODELS", pack.dir.join("models"))
            // 模型用过之后在内存里多留一会儿，免得每次提问都重新加载
            .env("OLLAMA_KEEP_ALIVE", "30m")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
    }

    /// 启动并看护推理服务。没有算力包、或端口上已有服务时什么都不启动。
    pub fn spawn(self: &Arc<Self>, db: Db) {
        let me = self.clone();
        tokio::spawn(async move {
            let Some(pack) = me.pack() else { return };
            if me.answering().await {
                me.set(State::External);
                return;
            }
            let mut failures = 0u32;
            loop {
                me.set(State::Starting);
                match me.launch(&pack) {
                    Err(e) => {
                        me.set(State::Failed { detail: format!("{}: {e}", pack.binary.display()) });
                        return;
                    }
                    Ok(child) => *me.child.lock().unwrap_or_else(|p| p.into_inner()) = Some(child),
                }
                // 等它开始应答；大机器上加载运行库可能要一会儿
                let mut up = false;
                for _ in 0..120 {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if me.answering().await {
                        up = true;
                        break;
                    }
                    if me.exited() {
                        break;
                    }
                }
                if up {
                    failures = 0;
                    me.set(State::Running);
                    if let Err(e) = assign_models(&db, &pack.info, &me.base_url()).await {
                        eprintln!("compute: could not assign models: {e}");
                    }
                    // 一直等到它退出（被我们停掉，或自己崩了）
                    while !me.exited() {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                if me.child.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
                    return; // 是我们主动停的
                }
                failures += 1;
                if failures >= 5 {
                    me.set(State::Failed { detail: format!("the runtime keeps exiting; see {}", me.log.display()) });
                    return;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    fn exited(&self) -> bool {
        match self.child.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        }
    }

    /// 停掉随包启动的推理服务（程序退出时调用）。别人启动的服务不动。
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = child.start_kill();
        }
    }
}

/// 第一次用上这个算力包时，把模型派好用场：默认对话模型、向量化模型、文字识别模型。
/// 每个包只做一次；用户之后自己改的设置不会被覆盖。
async fn assign_models(db: &Db, info: &PackInfo, base_url: &str) -> commonbot_core::Result<()> {
    let (info, base_url) = (info.clone(), base_url.to_string());
    db.run(move |c| {
        let marker = format!("{}|{}|{}|{}", info.built, info.chat, info.embed, info.vision);
        if store::get_setting(c, "compute.assigned")?.as_deref() == Some(marker.as_str()) {
            return Ok(());
        }
        // 找到指向本机推理服务的那一项；没有就建一个
        let existing = store::list_providers(c)?.into_iter().find(|p| p.base_url == base_url);
        let provider = match existing {
            Some(p) if p.default_model.is_none() && !info.chat.is_empty() => store::update_provider(
                c,
                &p.id,
                ProviderInput {
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    base_url: p.base_url.clone(),
                    api_key: None,
                    default_model: Some(info.chat.clone()),
                },
            )?,
            Some(p) => p,
            None => store::create_provider(
                c,
                ProviderInput {
                    name: "Ollama".into(),
                    kind: "local".into(),
                    base_url,
                    api_key: None,
                    default_model: (!info.chat.is_empty()).then(|| info.chat.clone()),
                },
            )?,
        };
        let models = kb::models(c)?;
        if models.embed.is_none() && !info.embed.is_empty() && kb::set_model(c, "embed", &provider.id, &info.embed)? {
            kb::requeue_all_for_embedding(c)?;
        }
        if models.ocr.is_none() && !info.vision.is_empty() && kb::set_model(c, "ocr", &provider.id, &info.vision)? {
            kb::requeue_waiting_for_ocr(c)?;
        }
        store::set_setting(c, "compute.assigned", &marker)
    })
    .await
}
