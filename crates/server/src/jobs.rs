//! 后台任务：回答在后台生成，不随浏览器的连接或页面切换而中断。
//! 每个对话同一时间只有一个回答在生成；界面可以随时接上去看进度，也可以叫停。
//! 任务中心读的就是这里的清单（加上知识库入库、算力下载的进度）。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};

/// 保留多少条已结束的任务给任务中心看。
const KEEP_FINISHED: usize = 30;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[derive(Default)]
struct Progress {
    sources: Option<Value>,
    fallback: Option<Value>,
    content: String,
    reasoning: String,
    /// 结束时的最后一个事件（done / error），以及结束的时间。
    finished: Option<(&'static str, Value, i64)>,
    status: &'static str,
}

/// 一个正在生成（或刚结束）的回答。
pub struct ChatJob {
    pub conversation_id: String,
    pub agent_id: String,
    pub agent_name: String,
    /// 这次的提问，给任务中心显示用。
    pub title: String,
    pub started_at: i64,
    progress: Mutex<Progress>,
    events: broadcast::Sender<(&'static str, Value)>,
    stop: watch::Sender<bool>,
}

/// 接上一个任务时拿到的东西：到目前为止的内容，以及之后的事件。
pub struct Attached {
    pub snapshot: Value,
    /// 已经结束的任务：最后那个事件。
    pub finished: Option<(&'static str, Value)>,
    pub events: broadcast::Receiver<(&'static str, Value)>,
}

impl ChatJob {
    fn lock(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn snapshot_of(p: &Progress) -> Value {
        json!({ "content": p.content, "reasoning": p.reasoning, "sources": p.sources, "fallback": p.fallback })
    }

    /// 记下一个事件并转给所有正在看的界面。
    pub fn publish(&self, name: &'static str, data: &Value) {
        let mut p = self.lock();
        match name {
            "sources" => p.sources = Some(data.clone()),
            "fallback" => p.fallback = Some(data.clone()),
            "delta" => p.content.push_str(data["text"].as_str().unwrap_or("")),
            "reasoning" => p.reasoning.push_str(data["text"].as_str().unwrap_or("")),
            _ => {}
        }
        // 在锁里发：接上来的界面拿到的「目前内容」和之后的事件正好衔接
        let _ = self.events.send((name, data.clone()));
    }

    pub fn attach(&self) -> Attached {
        let p = self.lock();
        Attached {
            snapshot: Self::snapshot_of(&p),
            finished: p.finished.as_ref().map(|(n, d, _)| (*n, d.clone())),
            events: self.events.subscribe(),
        }
    }

    pub fn snapshot(&self) -> Value {
        Self::snapshot_of(&self.lock())
    }

    /// 叫停。已经生成的部分照样保存。
    pub fn cancel(&self) {
        let _ = self.stop.send(true);
    }

    /// 等到被叫停。
    pub async fn cancelled(&self) {
        let mut rx = self.stop.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    fn summary(&self) -> Value {
        let p = self.lock();
        json!({
            "id": format!("chat-{}-{}", self.conversation_id, self.started_at),
            "kind": "chat",
            "status": if p.finished.is_some() { p.status } else { "running" },
            "title": self.title,
            "agent_id": self.agent_id,
            "agent_name": self.agent_name,
            "conversation_id": self.conversation_id,
            "started_at": self.started_at,
            "finished_at": p.finished.as_ref().map(|f| f.2),
        })
    }
}

#[derive(Default)]
pub struct Jobs {
    running: Mutex<HashMap<String, Arc<ChatJob>>>,
    finished: Mutex<VecDeque<Arc<ChatJob>>>,
}

impl Jobs {
    /// 给一个对话开一个任务。这个对话已经有回答在生成时返回 `None`。
    pub fn start(
        &self,
        conversation_id: &str,
        agent_id: &str,
        agent_name: &str,
        question: &str,
    ) -> Option<Arc<ChatJob>> {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if running.contains_key(conversation_id) {
            return None;
        }
        let job = Arc::new(ChatJob {
            conversation_id: conversation_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            title: question.chars().take(80).collect(),
            started_at: now_ms(),
            progress: Mutex::default(),
            events: broadcast::channel(256).0,
            stop: watch::channel(false).0,
        });
        running.insert(conversation_id.to_string(), job.clone());
        Some(job)
    }

    /// 这个对话里正在生成的回答。
    pub fn running(&self, conversation_id: &str) -> Option<Arc<ChatJob>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).get(conversation_id).cloned()
    }

    /// 任务结束：记下最后的事件，通知正在看的界面，挪到「已结束」里。
    /// `status`：done / error / stopped。没开始就失败的任务用 `discard`。
    pub fn finish(&self, job: &Arc<ChatJob>, name: &'static str, data: Value, status: &'static str) {
        {
            let mut p = job.lock();
            p.status = status;
            p.finished = Some((name, data.clone(), now_ms()));
            let _ = job.events.send((name, data));
        }
        self.running.lock().unwrap_or_else(|p| p.into_inner()).remove(&job.conversation_id);
        let mut finished = self.finished.lock().unwrap_or_else(|p| p.into_inner());
        finished.push_front(job.clone());
        finished.truncate(KEEP_FINISHED);
    }

    /// 任务还没真正开始就出了错：直接去掉，不进清单。
    pub fn discard(&self, job: &Arc<ChatJob>) {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).remove(&job.conversation_id);
    }

    /// 清掉已结束的任务。
    pub fn clear_finished(&self) {
        self.finished.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// 进行中的在前（新的在前），然后是已结束的。
    pub fn list(&self) -> Vec<Value> {
        let mut running: Vec<Arc<ChatJob>> =
            self.running.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        running.sort_by_key(|j| std::cmp::Reverse(j.started_at));
        let finished = self.finished.lock().unwrap_or_else(|p| p.into_inner());
        running.iter().chain(finished.iter()).map(|j| j.summary()).collect()
    }
}
