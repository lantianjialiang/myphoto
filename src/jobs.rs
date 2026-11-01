//! 持久化后台任务：进度、取消、状态记录。任务本身在独立线程执行。

use crate::db;
use crate::util;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
pub struct Progress {
    pub total: i64,
    pub done: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub skipped: i64,
    pub message: String,
    pub phase: String,
    pub finished: bool,
    pub error: Option<String>,
}

pub struct JobCtl {
    pub id: String,
    pub kind: String,
    pub db_path: PathBuf,
    cancel: AtomicBool,
    prog: Mutex<Progress>,
}

impl JobCtl {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn set_total(&self, total: i64) {
        if let Ok(mut p) = self.prog.lock() {
            p.total = total;
        }
    }

    pub fn phase(&self, msg: &str) {
        if let Ok(mut p) = self.prog.lock() {
            p.phase = msg.to_string();
            p.message = msg.to_string();
        }
    }

    pub fn ok(&self) {
        if let Ok(mut p) = self.prog.lock() {
            p.done += 1;
            p.succeeded += 1;
        }
    }

    pub fn skip(&self) {
        if let Ok(mut p) = self.prog.lock() {
            p.done += 1;
            p.skipped += 1;
        }
    }

    pub fn fail(&self) {
        if let Ok(mut p) = self.prog.lock() {
            p.done += 1;
            p.failed += 1;
        }
    }

    pub fn snapshot(&self) -> Progress {
        self.prog.lock().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn is_finished(&self) -> bool {
        self.prog.lock().map(|p| p.finished).unwrap_or(true)
    }

    pub fn to_json(&self) -> serde_json::Value {
        let p = self.snapshot();
        serde_json::json!({
            "id": self.id,
            "job_type": self.kind,
            "status": if p.finished {
                final_status(&p, self.is_cancelled())
            } else if self.is_cancelled() { "cancelling" } else { "running" },
            "total": p.total,
            "done": p.done,
            "succeeded": p.succeeded,
            "failed": p.failed,
            "skipped": p.skipped,
            "message": p.message,
            "phase": p.phase,
            "error": p.error,
        })
    }

    /// 收尾：写入 jobs 表最终状态。
    pub fn finish(&self, error: Option<String>) {
        if let Ok(mut p) = self.prog.lock() {
            p.finished = true;
            p.error = error.clone();
            if error.is_none() && self.is_cancelled() {
                p.message = format!("已取消，已处理 {}/{} 项（成功 {}，失败 {}）", p.done, p.total, p.succeeded, p.failed);
            }
        }
        let p = self.snapshot();
        let status = final_status(&p, self.is_cancelled());
        if let Ok(conn) = db::open(&self.db_path) {
            let _ = conn.execute(
                "UPDATE jobs SET status=?2, total=?3, done=?4, succeeded=?5, failed=?6, skipped=?7,
                 message=?8, error=?9, finished_at=?10 WHERE id=?1",
                rusqlite::params![
                    self.id,
                    status,
                    p.total,
                    p.done,
                    p.succeeded,
                    p.failed,
                    p.skipped,
                    p.message,
                    error,
                    util::now_iso()
                ],
            );
        }
    }
}

fn final_status(p: &Progress, cancelled: bool) -> &'static str {
    if p.error.is_some() { "failed" }
    else if cancelled { "cancelled" }
    else if p.failed > 0 && p.succeeded == 0 { "failed" }
    else if p.failed > 0 { "partial" }
    else { "completed" }
}

pub struct JobManager {
    inner: Mutex<HashMap<String, Arc<JobCtl>>>,
    db_path: PathBuf,
}

impl JobManager {
    pub fn new(db_path: PathBuf) -> JobManager {
        JobManager {
            inner: Mutex::new(HashMap::new()),
            db_path,
        }
    }

    /// 上电时把上次未完成的任务标记为中断（可重试），不猜测其完成状态。
    pub fn mark_interrupted(&self) {
        if let Ok(conn) = db::open(&self.db_path) {
            let _ = conn.execute(
                "UPDATE jobs SET status='interrupted', message=COALESCE(message,'')||' [上次退出时未完成]'
                 WHERE status IN ('running','queued','cancelling')",
                [],
            );
        }
    }

    pub fn start<F>(&self, kind: &str, label: &str, total: i64, f: F) -> String
    where
        F: FnOnce(Arc<JobCtl>) + Send + 'static,
    {
        let id = util::uuid();
        let ctl = Arc::new(JobCtl {
            id: id.clone(),
            kind: kind.to_string(),
            db_path: self.db_path.clone(),
            cancel: AtomicBool::new(false),
            prog: Mutex::new(Progress {
                total,
                message: label.to_string(),
                ..Default::default()
            }),
        });
        if let Ok(conn) = db::open(&self.db_path) {
            let _ = conn.execute(
                "INSERT INTO jobs(id,job_type,status,total,message,created_at,started_at)
                 VALUES(?1,?2,'running',?3,?4,?5,?5)",
                rusqlite::params![id, kind, total, label, util::now_iso()],
            );
        }
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id.clone(), ctl.clone());
        }
        let cloned = ctl.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(cloned.clone())));
            match result {
                // 任务体自己已经收尾（例如写入了失败原因）时不要覆盖它
                Ok(()) => {
                    if !cloned.is_finished() {
                        cloned.finish(None);
                    }
                }
                Err(_) => cloned.finish(Some("任务异常终止（程序内部错误）".into())),
            }
        });
        id
    }

    pub fn get(&self, id: &str) -> Option<Arc<JobCtl>> {
        self.inner.lock().ok().and_then(|m| m.get(id).cloned())
    }

    pub fn cancel(&self, id: &str) -> bool {
        match self.get(id) {
            Some(j) => {
                j.cancel();
                true
            }
            None => false,
        }
    }

    pub fn running(&self) -> Vec<Arc<JobCtl>> {
        self.inner
            .lock()
            .map(|m| {
                m.values()
                    .filter(|j| !j.snapshot().finished)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn cleanup(&self) {
        if let Ok(mut m) = self.inner.lock() {
            m.retain(|_, j| !j.snapshot().finished);
        }
    }
}
