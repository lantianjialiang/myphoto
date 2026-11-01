//! 源目录变更监控（轮询）。
//!
//! 不引入文件系统钩子：定时读取每个源根目录的 mtime（只在直接子项增删改名时变化），
//! 一旦变化就排队一次增量扫描。扫描本身是增量且只读的，未变化文件不会重读元数据。
//! 这样在没有管理员权限、NAS 或外接盘上也不会丢事件，代价是最长一个轮询周期的延迟。

use crate::api;
use crate::scan;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub struct Watcher {
    /// 每个源上次观察到的根目录 mtime（毫秒）
    seen: Mutex<HashMap<String, i64>>,
    /// 由监控排队、尚未结束的扫描任务
    inflight: Mutex<HashMap<String, String>>,
    stop: AtomicBool,
}

fn dir_mtime_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

fn setting_bool(app: &api::Shared, key: &str, default: bool) -> bool {
    let Ok(conn) = crate::db::open(&app.paths.db_path) else { return default };
    match crate::db::get_setting(&conn, key) {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        None => default,
    }
}

fn setting_i64(app: &api::Shared, key: &str, default: i64) -> i64 {
    let Ok(conn) = crate::db::open(&app.paths.db_path) else { return default };
    crate::db::get_setting(&conn, key)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

fn sources(app: &api::Shared) -> Vec<(String, String)> {
    let Ok(conn) = crate::db::open(&app.paths.db_path) else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare("SELECT id, root_path FROM sources") else { return Vec::new() };
    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// 启动监控线程（进程内单例）。返回的句柄仅用于停止。
pub fn start(app: api::Shared) -> Arc<Watcher> {
    let w = Arc::new(Watcher {
        seen: Mutex::new(HashMap::new()),
        inflight: Mutex::new(HashMap::new()),
        stop: AtomicBool::new(false),
    });
    let inner = w.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        // 首次先建立基线，避免启动瞬间把所有源重扫一遍
        for (id, root) in sources(&app2) {
            if let Some(m) = dir_mtime_ms(Path::new(&root)) {
                if let Ok(mut seen) = inner.seen.lock() {
                    seen.insert(id, m);
                }
            }
        }
        let mut last_tick = std::time::Instant::now();
        loop {
            if inner.stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
            if !setting_bool(&app2, "watch_enabled", false) {
                last_tick = std::time::Instant::now();
                continue;
            }
            let interval = setting_i64(&app2, "watch_interval", 300).clamp(30, 86_400);
            if last_tick.elapsed() < Duration::from_secs(interval as u64) {
                continue;
            }
            last_tick = std::time::Instant::now();
            tick(&app2, &inner);
        }
    });
    w
}

fn tick(app: &api::Shared, w: &Watcher) {
    // 回收已结束的排队记录
    if let Ok(mut inflight) = w.inflight.lock() {
        inflight.retain(|_, job_id| {
            app.jobs
                .get(job_id)
                .map(|j| !j.snapshot().finished)
                .unwrap_or(false)
        });
    }
    for (id, root) in sources(app) {
        let root_path = PathBuf::from(&root);
        let Some(m) = dir_mtime_ms(&root_path) else {
            continue; // 离线源不触发扫描
        };
        let changed = {
            let mut seen = match w.seen.lock() {
                Ok(v) => v,
                Err(_) => continue,
            };
            match seen.get(&id) {
                Some(prev) if *prev == m => false,
                _ => {
                    seen.insert(id.clone(), m);
                    true
                }
            }
        };
        if !changed {
            continue;
        }
        if w.inflight.lock().map(|s| s.contains_key(&id)).unwrap_or(false) {
            continue;
        }
        let data_dir = app.paths.data_dir.clone();
        let db_path = app.paths.db_path.clone();
        let source_id = id.clone();
        let root_for_log = root.clone();
        let job_id = app.jobs.start("scan", "监控到目录变化，增量扫描", 0, move |ctl| {
            if let Err(e) = scan::run(&db_path, &ctl, &source_id, &data_dir) {
                ctl.finish(Some(e));
            }
        });
        if let Ok(conn) = crate::db::open(&app.paths.db_path) {
            crate::db::log_op(
                &conn,
                "watch",
                Some(&root_for_log),
                None,
                None,
                None,
                "ok",
                Some(&job_id),
                Some("检测到源根目录变化，已排队增量扫描"),
            );
        }
        if let Ok(mut inflight) = w.inflight.lock() {
            inflight.insert(id, job_id);
        }
    }
}
