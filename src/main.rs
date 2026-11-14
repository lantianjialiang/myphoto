//! 本地照片与视频整理工具（离线 / 非破坏性）。
//!
//! 进程模型：单可执行文件启动本地回环 HTTP 服务（Rust 负责扫描、元数据、哈希、复制），
//! 界面为内嵌的 Web UI，通过 Edge/Chrome 的“应用窗口”打开，不依赖网络与任何在线资源。
//!
//! 发布版用 Windows 子系统编译：双击 exe 直接起界面，不留黑色控制台窗口；
//! 从命令行启动时会挂到父控制台，`--no-window` 的日志照旧可重定向到文件。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod album;
mod archive;
mod assets;
mod dateinfer;
mod db;
mod dedupe;
mod edit;
mod ffmpeg;
mod heif;
mod imgops;
mod jobs;
mod livepair;
mod media;
mod portable;
mod orientation;
mod query;
mod scan;
mod server;
mod similar;
mod slideshow;
mod tags;
mod trash;
mod util;
mod watch;
mod window_lifetime;
mod watermark;
mod watermark_detect;
mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 发布版是 GUI 子系统，从命令行启动时把标准输出接回父控制台，
/// 这样 `--no-window > server.log` 与直接在 cmd 里跑都能看到日志。
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// 致命错误：发布版没有控制台，用弹窗告诉用户，别让双击后“什么都没发生”。
fn fatal(headline: &str) -> ! {
    { use std::io::Write; let _ = writeln!(std::io::stderr(), "{headline}"); }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: *mut u8, text: *const u16, caption: *const u16, flags: u32) -> i32;
        }
        let to_wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
        };
        let text = to_wide(headline);
        let cap = to_wide("光匣 · 本地影像管家");
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), cap.as_ptr(), 0x10);
        }
    }
    std::process::exit(1);
}

/// 发布版没有控制台时 stdout 可能是无效句柄，`println!` 会直接 panic。
/// 统一走这个 helper：写不出去就当没这回事。
fn say(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{msg}");
}

fn find_browser() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PVM_BROWSER").map(PathBuf::from).filter(|p|p.is_file()) { return Some(path); }
    let candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("Microsoft\\Edge\\Application\\msedge.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn launch_window(url: &str, profile: &Path) -> Option<std::process::Child> {
    let browser = find_browser()?;
    let _ = std::fs::create_dir_all(profile);
    std::process::Command::new(browser)
        .arg(format!("--app={url}"))
        .arg("--start-maximized")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--disable-features=Translate,EdgeCollections")
        .spawn()
        .ok()
}

fn main() {
    attach_parent_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = args.iter().any(|a| a == "--no-window");
    let mut paths = db::AppPaths::resolve();
    if let Some(pos) = args.iter().position(|a| a == "--data-dir") {
        if let Some(dir) = args.get(pos + 1) {
            let data_dir = PathBuf::from(dir);
            paths = db::AppPaths {
                db_path: data_dir.join("library.db"),
                cache_dir: data_dir.join("cache"),
                log_dir: data_dir.join("logs"),
                data_dir,
            };
        }
    }
    for d in [&paths.data_dir, &paths.cache_dir, &paths.log_dir] {
        let _ = std::fs::create_dir_all(d);
    }
    // 发布版没有控制台，panic 只写进日志文件，方便事后定位（界面上只会看到一句“内部错误”）
    let panic_log = paths.log_dir.join("panic.log");
    std::panic::set_hook(Box::new(move |info| {
        let where_ = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        let line = format!("[{}] {where_}\n{info}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        eprintln!("{line}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&panic_log) {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }));
    // HEIC 解码器是可选的外置 DLL：找不到也不影响其它功能，只在需要时给出原因。
    heif::set_search_dirs(vec![paths.data_dir.join("lib")]);

    if let Err(e) = ffmpeg::prepare_bundled() {
        say(&format!("内置视频组件暂不可用：{e}"));
        let _ = std::fs::write(paths.log_dir.join("ffmpeg-setup.log"), &e);
    }

    if let Err(e) = portable::prepare(&paths, !args.iter().any(|a| a == "--data-dir")) {
        fatal(&format!("便携数据初始化失败：{e}\n请将程序放到可写的文件夹后重试。原数据保持不变。"));
    }
    if let Err(e) = db::open(&paths.db_path) {
        fatal(&format!("数据库初始化失败：{e}\n\n数据目录：{}", paths.data_dir.display()));
    }

    let job_manager = jobs::JobManager::new(paths.db_path.clone());
    job_manager.mark_interrupted();

    let app: api::Shared = Arc::new(api::App {
        paths: paths.clone(),
        jobs: job_manager,
        token: util::uuid(),
        version: VERSION.to_string(),
        windows: window_lifetime::WindowLifetime::default(),
    });

    // 目录变更监控（轮询，默认关闭；在设置里开启后按间隔增量扫描）
    let _watcher = watch::start(app.clone());

    let handle = match server::start(app.clone()) {
        Ok(h) => h,
        Err(e) => fatal(&format!("无法启动本地服务：{e}")),
    };
    let url = format!("http://127.0.0.1:{}/?token={}", handle.port, app.token);
    say(&format!("光匣 · 本地影像管家 v{VERSION}"));
    say(&format!("数据目录：{}", paths.data_dir.display()));
    say(&format!("本地地址：{url}"));

    if headless {
        say("已以无窗口模式运行（Ctrl+C 退出）。");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    match launch_window(&url, &paths.data_dir.join("window-profile")) {
        Some(mut child) => {
            // Browser launchers may exit while the actual app window remains open.
            let mut launcher_finished = false;
            let mut last_tick = std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(600));
                let now = std::time::Instant::now();
                if now.duration_since(last_tick) > Duration::from_secs(30) { app.windows.resumed(); }
                last_tick = now;
                if !launcher_finished && matches!(child.try_wait(),Ok(Some(_))) {
                    launcher_finished = true;
                    say("浏览器启动进程已退出，继续为应用窗口提供服务。");
                }
                if app.windows.should_exit(now) && app.jobs.running().is_empty() { break; }
            }
            say("窗口已关闭，程序退出。");
        }
        None => {
            say("未找到 Edge/Chrome，已改用默认浏览器打开。按 Ctrl+C 结束程序。");
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .spawn();
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    }
}
