//! 本地 HTTP API。仅监听 127.0.0.1，并要求启动时生成的一次性 token。

use crate::album;
use crate::archive;
use crate::assets;
use crate::dateinfer;
use crate::db;
use crate::dedupe;
use crate::jobs::JobManager;
use crate::livepair;
use crate::media;
use crate::query;
use crate::scan;
use crate::similar;
use crate::tags;
use crate::util;
use crate::watermark;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, StatusCode};

pub struct App {
    pub paths: db::AppPaths,
    pub jobs: JobManager,
    pub token: String,
    pub version: String,
    pub windows: crate::window_lifetime::WindowLifetime,
}

pub type Shared = Arc<App>;

pub type Body = Box<dyn Read + Send>;

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

pub fn respond_bytes(
    status: u16,
    body: Vec<u8>,
    content_type: &str,
    extra: Vec<(String, String)>,
) -> Response<Body> {
    let len = body.len();
    let mut headers = vec![
        header("Content-Type", content_type),
        header("X-Content-Type-Options", "nosniff"),
    ];
    for (k, v) in extra {
        headers.push(header(&k, &v));
    }
    Response::new(
        StatusCode(status),
        headers,
        Box::new(Cursor::new(body)) as Body,
        Some(len),
        None,
    )
}

fn json_ok(value: Value) -> Response<Body> {
    respond_bytes(200, value.to_string().into_bytes(), "application/json; charset=utf-8", vec![])
}

fn json_err(status: u16, msg: &str) -> Response<Body> {
    respond_bytes(
        status,
        json!({"ok": false, "error": msg}).to_string().into_bytes(),
        "application/json; charset=utf-8",
        vec![],
    )
}

fn ok<T: Into<Value>>(value: T) -> Response<Body> {
    json_ok(json!({"ok": true, "result": value.into()}))
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn parse_url(url: &str) -> (String, std::collections::HashMap<String, String>) {
    let mut parts = url.splitn(2, '?');
    let path = url_decode(parts.next().unwrap_or("/"));
    let mut map = std::collections::HashMap::new();
    if let Some(q) = parts.next() {
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut kv = pair.splitn(2, '=');
            let k = url_decode(kv.next().unwrap_or(""));
            let v = url_decode(kv.next().unwrap_or(""));
            map.insert(k, v);
        }
    }
    (path, map)
}

fn qs(map: &std::collections::HashMap<String, String>, key: &str) -> Option<String> {
    map.get(key).cloned().filter(|v| !v.is_empty())
}

fn qb(map: &std::collections::HashMap<String, String>, key: &str) -> Option<bool> {
    map.get(key).map(|v| v == "1" || v == "true")
}

fn filter_from(map: &std::collections::HashMap<String, String>) -> query::Filter {
    query::Filter {
        q: qs(map, "q"),
        media_type: qs(map, "media_type"),
        source_id: qs(map, "source_id"),
        year: qs(map, "year"),
        month: qs(map, "month"),
        date_from: qs(map, "date_from"),
        date_to: qs(map, "date_to"),
        confidence: qs(map, "confidence"),
        unknown_date: qb(map, "unknown_date"),
        needs_review: qb(map, "needs_review"),
        dup: qb(map, "dup"),
        missing: qb(map, "missing"),
        marked: qb(map, "marked"),
        camera: qs(map, "camera"),
        star: qb(map, "star").filter(|v| *v),
        favorite: qb(map, "favorite").filter(|v| *v),
        live: qb(map, "live").filter(|v| *v),
        tag: qs(map, "tag"),
        job_id: qs(map, "job_id"),
        watermarked: map.get("watermarked").map(|v|v=="1" || v=="true"),
    }
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "wmv" => "video/x-ms-wmv",
        "3gp" => "video/3gpp",
        _ => "application/octet-stream",
    }
}

fn read_body(req: &mut Request) -> Option<Value> {
    let mut buf = Vec::new();
    let limit = 8 * 1024 * 1024;
    let mut reader = req.as_reader().take(limit as u64);
    reader.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return Some(Value::Null);
    }
    serde_json::from_slice(&buf).ok()
}

fn open_db(app: &Shared) -> Result<Connection, Response<Body>> {
    db::open(&app.paths.db_path).map_err(|e| json_err(500, &format!("数据库不可用：{e}")))
}

pub fn handle(app: &Shared, mut req: Request) {
    let url = req.url().to_string();
    // 单个请求 panic 不能让处理线程整个死掉（否则连点几下界面就“连不上本地服务”了）
    let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        route_all(app, &mut req)
    })) {
        Ok(r) => r,
        Err(p) => {
            let msg = panic_text(&p);
            {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "[pvm] 请求处理失败 {url}：{msg}");
            }
            json_err(500, &format!("内部错误：{msg}"))
        }
    };
    let _ = req.respond(resp);
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知错误".to_string()
    }
}

fn route_all(app: &Shared, req: &mut Request) -> Response<Body> {
    let (path, params) = parse_url(req.url());
    // 防 DNS rebinding：只接受回环 Host
    let host_ok = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("host"))
        .map(|h| {
            let v = h.value.as_str().to_lowercase();
            v.starts_with("127.0.0.1") || v.starts_with("localhost") || v.starts_with("[::1]")
        })
        .unwrap_or(false);
    if !host_ok {
        return json_err(403, "仅允许本机访问");
    }

    // 静态资源不需要 token（不含任何用户数据）
    match path.as_str() {
        "/" | "/index.html" => {
            return respond_bytes(
                200,
                crate::web::INDEX.as_bytes().to_vec(),
                "text/html; charset=utf-8",
                vec![("Cache-Control".into(), "no-store".into())],
            )
        }
        "/app.js" => {
            return respond_bytes(
                200,
                crate::web::APP_JS.as_bytes().to_vec(),
                "application/javascript; charset=utf-8",
                // 必须禁用缓存：升级 exe 后浏览器若还用旧脚本，会出现「接口不存在」这类怪问题
                vec![("Cache-Control".into(), "no-store, must-revalidate".into())],
            )
        }
        "/style.css" => {
            return respond_bytes(
                200,
                crate::web::STYLE_CSS.as_bytes().to_vec(),
                "text/css; charset=utf-8",
                vec![("Cache-Control".into(), "no-store, must-revalidate".into())],
            )
        }
        "/favicon.ico" => {
            return respond_bytes(200, crate::web::FAVICON.to_vec(), "image/png", vec![])
        }
        _ => {}
    }

    let token_ok = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("x-pvm-token"))
        .map(|h| h.value.as_str() == app.token)
        .unwrap_or(false)
        || params.get("token").map(|t| t == &app.token).unwrap_or(false);
    if !token_ok {
        return json_err(401, "令牌无效");
    }

    let method = req.method().clone();
    let segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();

    let result = route(app, req, &method, &segments, &params);
    match result {
        Some(resp) => resp,
        None => json_err(404, "接口不存在"),
    }
}

fn route(
    app: &Shared,
    req: &mut Request,
    method: &Method,
    seg: &[&str],
    params: &std::collections::HashMap<String, String>,
) -> Option<Response<Body>> {
    let m = method.as_str();
    match seg {
        ["api", "state"] => Some(api_state(app)),
        ["api", "sources"] if m == "GET" => Some(api_sources(app)),
        ["api", "sources"] if m == "POST" => Some(api_source_add(app, req)),
        ["api", "sources", id] if m == "PATCH" => Some(api_source_patch(app, req, id)),
        ["api", "sources", id] if m == "DELETE" => Some(api_source_remove(app, id)),
        ["api", "sources", id, "scan"] if m == "POST" => Some(api_scan(app, id)),
        ["api", "sources", id, "reveal"] if m == "POST" => Some(api_source_reveal(app, id)),
        ["api", "media"] => Some(api_media_list(app, params)),
        ["api", "media", "batch-date"] if m == "POST" => Some(api_batch_date(app, req)),
        ["api", "media", "batch-tags"] if m == "POST" => Some(api_batch_tags(app, req)),
        ["api", "media", "batch-flags"] if m == "POST" => Some(api_batch_flags(app, req)),
        ["api", "media", "shift"] if m == "POST" => Some(api_shift(app, req)),
        ["api", "media", "ids"] => Some(api_media_ids(app, params)),
        ["api", "media", id] => Some(api_media_detail(app, id)),
        ["api", "media", id, "date"] if m == "POST" => Some(api_set_date(app, req, id)),
        ["api", "media", id, "refresh"] if m == "POST" => Some(api_refresh(app, id)),
        ["api", "media", id, "mark"] if m == "POST" => Some(api_mark(app, req, id)),
        ["api", "media", id, "tags"] if m == "POST" => Some(api_set_tags(app, req, id)),
        ["api", "media", id, "livepair"] if m == "POST" => Some(api_set_livepair(app, req, id)),
        ["api", "media", id, "reveal"] if m == "POST" => Some(api_reveal(app, req, id)),
        ["api", "thumb", id] => Some(api_thumb(app, id, params)),
        ["api", "thumb", id, "poster"] if m == "POST" => Some(api_poster(app, req, id)),
        ["api", "file", id] => Some(api_file(app, req, id)),
        ["api", "file"] => Some(json_err(400, "缺少文件 ID")),
        ["api", "duplicates"] => Some(api_duplicates(app)),
        ["api", "hash"] if m == "POST" => Some(api_hash(app, req)),
        ["api", "similar"] => Some(api_similar(app, params)),
        ["api", "similar", "hash"] if m == "POST" => Some(api_similar_hash(app)),
        ["api", "livepairs"] => Some(api_livepairs(app)),
        ["api", "livepairs", "run"] if m == "POST" => Some(api_livepairs_run(app)),
        ["api", "tags"] if m == "GET" => Some(api_tags(app)),
        ["api", "tags"] if m == "POST" => Some(api_tag_create(app, req)),
        ["api", "tags", id] if m == "PATCH" => Some(api_tag_update(app, req, id)),
        ["api", "tags", id] if m == "DELETE" => Some(api_tag_delete(app, id)),
        ["api", "fonts"] => Some(api_fonts()),
        ["api", "fonts", "user"] => Some(api_user_fonts(app)),
        ["api", "fonts", "validate"] if m == "POST" => Some(api_validate_font(app, req)),
        ["api", "watermark", "presets"] if m == "GET" => Some(api_wm_presets(app)),
        ["api", "watermark", "presets"] if m == "POST" => Some(api_wm_preset_save(app, req)),
        ["api", "watermark", "presets", id] if m == "DELETE" => Some(api_wm_preset_delete(app, id)),
        ["api", "watermark", "backups"] if m == "GET" => Some(api_wm_backups(app)),
        ["api", "watermark", "backups", "restore"] if m == "POST" => Some(api_wm_restore(app, req)),
        ["api", "watermark", "backups", "drop"] if m == "POST" => Some(api_wm_drop_backup(app, req)),
        ["api", "edit", "run"] if m == "POST" => Some(api_edit_run(app, req)),
        ["api", "edit", "preview"] if m == "POST" => Some(api_edit_preview(app, req)),
        ["api", "edit", "tone-presets"] => Some(ok(crate::imgops::tone_presets())),
        ["api", "edit", "backups"] if m == "GET" => Some(api_edit_backups(app)),
        ["api", "edit", "backups", "restore"] if m == "POST" => Some(api_edit_restore(app, req)),
        ["api", "edit", "backups", "drop"] if m == "POST" => Some(api_edit_drop(app, req)),
        ["api", "watermark", "preview"] if m == "POST" => Some(api_wm_preview(app, req)),
        ["api", "watermark", "run"] if m == "POST" => Some(api_wm_run(app, req)),
        ["api", "watermark", "mark"] if m == "POST" => Some(api_wm_mark(app,req)),
        ["api", "watermark", "rotation"] if m == "POST" => Some(api_wm_rotation(app,req)),
        ["api", "album", "run"] if m == "POST" => Some(api_album_run(app, req)),
        ["api", "image", id] if m == "GET" => Some(api_image(app, id)),
        ["api", "album", "themes"] => Some(ok(album::theme_list())),
        ["api", "slideshow", "run"] if m == "POST" => Some(api_slideshow_run(app, req)),
        ["api", "slideshow", "formats"] => Some(ok(crate::slideshow::formats())),
        ["api", "review"] => Some(api_review(app, params)),
        ["api", "archive", "plan"] if m == "POST" => Some(api_plan(app, req)),
        ["api", "archive", "plans"] => Some(api_plans(app)),
        ["api", "archive", "plans", id] => Some(api_get_plan(app, id)),
        ["api", "archive", "plans", id, "run"] if m == "POST" => Some(api_run_plan(app, id)),
        ["api", "archive", "plans", id, "undo"] if m == "POST" => Some(api_undo_plan(app, id)),
        ["api", "jobs"] => Some(api_jobs(app)),
        ["api", "jobs", id, "cancel"] if m == "POST" => Some(api_job_cancel(app, id)),
        ["api", "jobs", id] => Some(api_job_get(app, id)),
        ["api", "logs"] => Some(api_logs(app, params)),
        ["api", "settings"] if m == "GET" => Some(api_settings_get(app)),
        ["api", "settings"] if m == "POST" => Some(api_settings_set(app, req)),
        ["api", "backup"] if m == "POST" => Some(api_backup(app)),
        ["api", "export", "save"] if m == "POST" => {
 let body=read_body(req).unwrap_or(Value::Null);
 let map=body.as_object().map(|obj|obj.iter().map(|(k,v)|(k.clone(),v.as_str().map(str::to_owned).unwrap_or_else(||v.to_string()))).collect()).unwrap_or_default();
 Some(api_export(app,&map,true))
 },
        ["api", "export"] => Some(api_export(app, params, false)),
        ["api", "facets"] => Some(api_facets(app)),
        ["api", "capabilities"] => Some(api_capabilities()),
        ["api", "pick-folder"] if m == "POST" => Some(api_pick_folder(req)),
        ["api", "window", action] if m == "POST" && matches!(*action,"heartbeat"|"close") => {
            let body = read_body(req).unwrap_or(Value::Null);
            match body.get("window_id").and_then(Value::as_str).filter(|id|!id.is_empty() && id.len()<=128) {
                Some(id) => { app.windows.update(id,*action=="close"); Some(ok(json!(true))) },
                None => Some(json_err(400,"缺少窗口标识")),
            }
        },
        _ => None,
    }
}

fn api_capabilities() -> Response<Body> {
    let heic = crate::heif::status();
    let ff = crate::ffmpeg::status();
    let codecs = [
        "jpeg", "png", "webp", "gif", "bmp", "tiff",
    ];
    json_ok(json!({
        "ok": true,
        "result": {
            "images": codecs,
            "heic": {
                "ok": heic.ok,
                "detail": heic.detail,
                "hint": if heic.ok { "" } else { "把 heif.dll 与 libde265.dll 放到程序目录的 lib\\ 下，或用 PVM_HEIF_DLL 指向 heif.dll" },
            },
            "ffmpeg": {
                "ok": ff.ok,
                "path": ff.path,
                "version": ff.version,
                "hint": ff.hint,
            },
            "video": {
                "container": ["mp4", "mov", "m4v", "avi", "mkv", "wmv", "3gp"],
                "note": "播放能力取决于浏览器内核；HEVC 需要系统安装 HEVC 扩展",
                "watermark": ff.ok,
            }
        }
    }))
}

fn api_state(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else {
        return json_err(500, "数据库不可用");
    };
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let assets_total = count("SELECT count(*) FROM media_assets");
    let images = count("SELECT count(*) FROM media_assets WHERE media_type='image'");
    let videos = count("SELECT count(*) FROM media_assets WHERE media_type='video'");
    let unknown = count("SELECT count(*) FROM media_assets WHERE effective_taken_at IS NULL");
    let review = count("SELECT count(*) FROM media_assets WHERE needs_review=1");
    let files = count("SELECT count(*) FROM media_files");
    let missing = count("SELECT count(*) FROM media_files WHERE availability='missing'");
    let bytes = count("SELECT COALESCE(sum(size_bytes),0) FROM media_files");
    let sources = count("SELECT count(*) FROM sources");
    let dup_groups = count(
        "SELECT count(*) FROM (SELECT sha256 FROM media_files WHERE sha256 IS NOT NULL
         GROUP BY sha256 HAVING count(*)>1)",
    );
    let free = util::free_space(&app.paths.data_dir).unwrap_or(0);
    let running = app.jobs.running().len();
    let starred = count("SELECT count(*) FROM media_assets WHERE star=1");
    let favorites = count("SELECT count(*) FROM media_assets WHERE favorite=1");
    let tags_total = count("SELECT count(*) FROM tags");
    let live_pairs = livepair::count(&conn);
    let tagged = count(
        "SELECT count(*) FROM media_assets a WHERE EXISTS
         (SELECT 1 FROM asset_tags at WHERE at.asset_id=a.id)",
    );
    let phash = count("SELECT count(*) FROM media_assets WHERE phash IS NOT NULL");
    json_ok(json!({
        "ok": true,
        "result": {
            "version": app.version,
            "data_dir": app.paths.data_dir.to_string_lossy(),
            "db_path": app.paths.db_path.to_string_lossy(),
            "assets": assets_total, "images": images, "videos": videos,
            "unknown_date": unknown, "needs_review": review,
            "files": files, "missing": missing, "bytes": bytes,
            "sources": sources, "dup_groups": dup_groups,
            "free_space": free, "running_jobs": running,
            "starred": starred, "favorites": favorites,
            "tags": tags_total, "tagged": tagged,
            "live_pairs": live_pairs, "phash": phash,
            "settings": settings_map(&conn),
        }
    }))
}

fn settings_map(conn: &Connection) -> Value {
    let mut stmt = match conn.prepare("SELECT key,value FROM settings") {
        Ok(s) => s,
        Err(_) => return json!({}),
    };
    let items: std::collections::BTreeMap<String, String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    json!(items)
}

fn api_facets(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match query::facets(&conn) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

/// 调起 Windows 原生文件夹选择框（PowerShell + WinForms，无需额外依赖）。
fn api_pick_folder(req: &mut Request) -> Response<Body> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    static PICKER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_guard) = PICKER.try_lock() else { return json_err(409,"已有文件夹选择窗口打开，请先完成或取消选择") };
    let body = read_body(req).unwrap_or(Value::Null);
    let initial = body.get("initial_path").and_then(Value::as_str).unwrap_or("");
    let kind = body.get("kind").and_then(Value::as_str).filter(|k|matches!(*k,"file"|"font")).unwrap_or("folder");
    #[link(name = "user32")]
    extern "system" { fn GetForegroundWindow() -> *mut std::ffi::c_void; }
    let owner = unsafe { GetForegroundWindow() } as usize;
    let script = include_str!("folder_picker.ps1");
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", script])
        .env("PVM_PICKER_OWNER",owner.to_string())
        .env("PVM_PICKER_INITIAL",initial)
        .env("PVM_PICKER_KIND",kind)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) => {
            if !o.status.success() {
                return json_err(500,&format!("无法打开文件夹选择框：{}",String::from_utf8_lossy(&o.stderr).trim()));
            }
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ok(json!({"path": if path.is_empty() { Value::Null } else { Value::String(path) }}))
        }
        Err(e) => json_err(500, &format!("无法打开文件夹选择框：{e}")),
    }
}

fn source_json(conn: &Connection, id: &str, root: &str, label: Option<String>, online: bool, last: Option<String>, options: String) -> Value {
    let media: i64 = conn
        .query_row(
            "SELECT count(*) FROM media_files WHERE source_id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let opts: scan::SourceOptions = serde_json::from_str(&options).unwrap_or_default();
    json!({
        "id": id,
        "root_path": root,
        "label": label,
        "online": online && Path::new(root).is_dir(),
        "is_online_db": online,
        "last_scan_at": last,
        "media_count": media,
        "options": opts,
        "exists": Path::new(root).is_dir(),
    })
}

fn api_sources(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let mut stmt = match conn.prepare("SELECT id,root_path,label,is_online,last_scan_at,options_json FROM sources ORDER BY root_path") {
        Ok(s) => s,
        Err(e) => return json_err(500, &e.to_string()),
    };
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .map(|(id, root, label, online, last, opts)| {
                    source_json(&conn, &id, &root, label, online != 0, last, opts)
                })
                .collect()
        })
        .unwrap_or_default();
    ok(json!(rows))
}

fn api_source_add(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let Some(path) = body.get("path").and_then(|v| v.as_str()) else {
        return json_err(400, "缺少路径");
    };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => util::clean_path(&p),
        Err(e) => return json_err(400, &format!("无法访问该文件夹：{e}")),
    };
    if !canonical.is_dir() {
        return json_err(400, "请选择文件夹（不是文件）");
    }
    if util::path_is_inside(&canonical, &app.paths.data_dir)
        || util::path_is_inside(&app.paths.data_dir, &canonical)
    {
        return json_err(400, "媒体源不能与应用数据目录重叠");
    }
    let root = canonical.to_string_lossy().to_string();
    let exists: Option<String> = conn
        .query_row("SELECT id FROM sources WHERE root_path=?1", params![root], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    if exists.is_some() {
        return json_err(400, "该文件夹已在媒体源列表中");
    }
    let id = util::uuid();
    let now = util::now_iso();
    if let Err(e) = conn.execute(
        "INSERT INTO sources(id,root_path,label,is_online,options_json,created_at) VALUES(?1,?2,NULL,1,'{}',?3)",
        params![id, root, now],
    ) {
        return json_err(500, &e.to_string());
    }
    ok(json!({"id": id, "root_path": root}))
}

fn api_source_patch(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let mut opts: scan::SourceOptions = conn
        .query_row("SELECT options_json FROM sources WHERE id=?1", params![id], |r| r.get::<_, String>(0))
        .optional()
        .unwrap_or(None)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(arr) = body.get("excludes").and_then(|v| v.as_array()) {
        opts.excludes = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    }
    if let Some(arr) = body.get("ext_excludes").and_then(|v| v.as_array()) {
        opts.ext_excludes = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    }
    let label = body.get("label").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Err(e) = conn.execute(
        "UPDATE sources SET options_json=?2, label=COALESCE(?3,label) WHERE id=?1",
        params![id, serde_json::to_string(&opts).unwrap_or_else(|_| "{}".into()), label],
    ) {
        return json_err(500, &e.to_string());
    }
    ok(json!(true))
}

fn api_source_remove(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let files: Vec<String> = {
        let mut stmt = match conn.prepare("SELECT absolute_path FROM media_files WHERE source_id=?1") {
            Ok(s) => s,
            Err(e) => return json_err(500, &e.to_string()),
        };
        stmt.query_map(params![id], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    let asset_ids: Vec<String> = {
        let mut stmt = match conn.prepare("SELECT DISTINCT asset_id FROM media_files WHERE source_id=?1") {
            Ok(s) => s,
            Err(e) => return json_err(500, &e.to_string()),
        };
        stmt.query_map(params![id], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    if let Err(e) = conn.execute("DELETE FROM sources WHERE id=?1", params![id]) {
        return json_err(500, &e.to_string());
    }
    // 删除失去所有文件引用的资产与预览缓存（仅索引，不动原文件）
    for asset_id in asset_ids {
        let left: i64 = conn
            .query_row(
                "SELECT count(*) FROM media_files WHERE asset_id=?1",
                params![asset_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if left == 0 {
            let _ = conn.execute("DELETE FROM media_assets WHERE id=?1", params![asset_id]);
            let _ = conn.execute("DELETE FROM media_previews WHERE asset_id=?1", params![asset_id]);
        }
    }
    db::log_op(
        &conn,
        "source-remove",
        Some(&format!("source:{id}")),
        None,
        None,
        None,
        "ok",
        None,
        Some(&format!("移除索引 {} 个文件；原文件未改动", files.len())),
    );
    ok(json!({"removed_files": files.len()}))
}

fn api_source_reveal(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let root: Option<String> = conn
        .query_row("SELECT root_path FROM sources WHERE id=?1", params![id], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    match root {
        Some(p) => match util::open_path(Path::new(&p)) {
            Ok(()) => ok(json!(true)),
            Err(e) => json_err(500, &e.to_string()),
        },
        None => json_err(404, "媒体源不存在"),
    }
}

fn api_scan(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let root: Option<String> = conn
        .query_row("SELECT root_path FROM sources WHERE id=?1", params![id], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    let Some(root) = root else { return json_err(404, "媒体源不存在") };
    if !Path::new(&root).is_dir() {
        let _ = conn.execute("UPDATE sources SET is_online=0 WHERE id=?1", params![id]);
        return json_err(409, "媒体源离线或不可访问（索引已保留，不会判定为删除）");
    }
    let db_path = app.paths.db_path.clone();
    let data_dir = app.paths.data_dir.clone();
    let source_id = id.to_string();
    let job = app.jobs.start("scan", &format!("扫描 {root}"), 0, move |ctl| {
        if let Err(e) = scan::run(&db_path, &ctl, &source_id, &data_dir) {
            ctl.phase(&e);
            ctl.finish(Some(e));
        }
    });
    ok(json!({"job_id": job}))
}

fn api_media_list(app: &Shared, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let filter = filter_from(params);
    let offset: i64 = qs(params, "offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: i64 = qs(params, "limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let sort = qs(params, "sort").unwrap_or_else(|| "taken".into());
    let order = qs(params, "order").unwrap_or_else(|| "desc".into());
    match query::list(&conn, &filter, &sort, &order, offset, limit) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn api_media_ids(app: &Shared, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let filter = filter_from(params);
    match query::ids(&conn, &filter) {
        Ok(v) => ok(json!(v)),
        Err(e) => json_err(500, &e),
    }
}

fn api_media_detail(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match assets::asset_json(&conn, id, true) {
        Ok(Some(mut v)) => {
            v["live_companion"] = livepair::companion(&conn, id).unwrap_or(None).unwrap_or(Value::Null);
            ok(v)
        }
        Ok(None) => json_err(404, "媒体不存在"),
        Err(e) => json_err(500, &e),
    }
}

fn apply_override(app: &Shared, ids: &[String], date: Option<String>) -> Result<i64, String> {
    let conn = db::open(&app.paths.db_path).map_err(|e| e.to_string())?;
    let mut n = 0i64;
    db::begin_write(&conn)?;
    for id in ids {
        conn.execute(
            "UPDATE media_assets SET override_taken_at=?2 WHERE id=?1",
            params![id, date],
        )
        .map_err(|e| e.to_string())?;
        assets::recompute(&conn, id)?;
        n += 1;
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    db::log_op(
        &conn,
        "date-override",
        None,
        None,
        None,
        None,
        "ok",
        None,
        Some(&format!("{} 个媒体，日期 {:?}", n, date)),
    );
    Ok(n)
}

fn parse_date_input(v: Option<&Value>) -> Result<Option<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let t = s.trim();
            if let Some(dt) = dateinfer::parse_iso(t) {
                Ok(Some(dateinfer::iso(dt)))
            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
                Ok(Some(format!("{d}T00:00:00")))
            } else {
                Err(format!("无法解析日期：{s}"))
            }
        }
        _ => Err("日期格式错误".into()),
    }
}

fn api_set_date(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let date = match parse_date_input(body.get("date")) {
        Ok(d) => d,
        Err(e) => return json_err(400, &e),
    };
    match apply_override(app, &[id.to_string()], date) {
        Ok(n) => ok(json!({"updated": n})),
        Err(e) => json_err(500, &e),
    }
}

fn api_batch_date(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return json_err(400, "没有选择媒体");
    }
    let date = match parse_date_input(body.get("date")) {
        Ok(d) => d,
        Err(e) => return json_err(400, &e),
    };
    match apply_override(app, &ids, date) {
        Ok(n) => ok(json!({"updated": n})),
        Err(e) => json_err(500, &e),
    }
}

fn api_refresh(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let files = match assets::files_of_asset(&conn, id) {
        Ok(f) => f,
        Err(e) => return json_err(500, &e),
    };
    if files.is_empty() {
        return json_err(404, "媒体不存在");
    }
    for f in &files {
        let dirs: Vec<String> = f
            .rel_dir
            .as_deref()
            .unwrap_or("")
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let ex = media::extract(
            Path::new(&f.absolute_path),
            &f.ext,
            util::media_kind(&f.ext).unwrap_or("image"),
            &dirs,
            f.mtime_ms,
        );
        let sel = dateinfer::select(&ex.candidates, None);
        let meta = json!({
            "width": ex.width, "height": ex.height, "duration_ms": ex.duration_ms,
            "camera": ex.camera,
            "gps_lat": ex.gps.map(|g| g.0), "gps_lon": ex.gps.map(|g| g.1),
            "error": ex.error, "details": ex.details,
        });
        let _ = conn.execute(
            "UPDATE media_files SET metadata_json=?2, candidates_json=?3, taken_at=?4,
             date_source=?5, date_confidence=?6 WHERE id=?1",
            params![
                f.id,
                meta.to_string(),
                serde_json::to_string(&ex.candidates).unwrap_or_else(|_| "[]".into()),
                sel.effective,
                sel.source,
                sel.confidence
            ],
        );
    }
    match assets::recompute(&conn, id) {
        Ok(()) => {
            let _ = conn.execute("DELETE FROM media_previews WHERE asset_id=?1", params![id]);
            match assets::asset_json(&conn, id, true) {
                Ok(Some(v)) => ok(v),
                _ => json_err(500, "重算失败"),
            }
        }
        Err(e) => json_err(500, &e),
    }
}

fn api_mark(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let marked = body.get("marked").and_then(|v| v.as_bool()).unwrap_or(false);
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let _ = conn.execute(
        "UPDATE media_files SET marked=?2 WHERE asset_id=?1",
        params![id, if marked { 1 } else { 0 }],
    );
    ok(json!({"marked": marked}))
}

/* ---------------- 标签 / 星标 / 时间偏移 ---------------- */

fn api_tags(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match tags::list(&conn) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn str_list(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// 取请求里的资产 id：显式 `ids` 优先，否则用 `filter` 解析为“筛选结果全选”。
fn ids_from_body(conn: &Connection, body: &Value) -> Result<Vec<String>, String> {
    let ids = str_list(body, "ids");
    if !ids.is_empty() {
        return Ok(ids);
    }
    if let Some(f) = body.get("filter") {
        if let Ok(filter) = serde_json::from_value::<query::Filter>(f.clone()) {
            return query::ids(conn, &filter);
        }
    }
    Ok(Vec::new())
}

fn api_tag_create(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let color = body.get("color").and_then(|v| v.as_str());
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match tags::create(&conn, name, color) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_tag_update(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let name = body.get("name").and_then(|v| v.as_str());
    let color = body.get("color").and_then(|v| v.as_str());
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match tags::update(&conn, id, name, color) {
        Ok(()) => ok(json!(true)),
        Err(e) => json_err(400, &e),
    }
}

fn api_tag_delete(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match tags::delete(&conn, id) {
        Ok(()) => ok(json!(true)),
        Err(e) => json_err(500, &e),
    }
}

fn api_set_tags(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let names = str_list(&body, "tags");
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match tags::set_asset(&conn, id, &names) {
        Ok(n) => ok(json!({"count": n, "tags": tags::of_asset(&conn, id).unwrap_or_default()})),
        Err(e) => json_err(400, &e),
    }
}

fn api_batch_tags(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("add");
    let names = str_list(&body, "tags");
    if names.is_empty() {
        return json_err(400, "请提供至少一个标签");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选择媒体");
    }
    match tags::mutate(&conn, &ids, &names, mode) {
        Ok(n) => {
            let _ = db::log_op(
                &conn,
                "tag",
                None,
                None,
                None,
                None,
                "ok",
                None,
                Some(&format!("{} 项 · {} 个标签 · 模式 {mode}", n, names.len())),
            );
            ok(json!({"updated": n, "tags": tags::list(&conn).unwrap_or(json!([]))}))
        }
        Err(e) => json_err(400, &e),
    }
}

fn api_batch_flags(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let star = body.get("star").and_then(|v| v.as_bool());
    let favorite = body.get("favorite").and_then(|v| v.as_bool());
    if star.is_none() && favorite.is_none() {
        return json_err(400, "未指定要修改的标记");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选择媒体");
    }
    match tags::set_flags(&conn, &ids, star, favorite) {
        Ok(n) => ok(json!({"updated": n})),
        Err(e) => json_err(500, &e),
    }
}

fn api_shift(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let minutes = body.get("minutes").and_then(|v| v.as_i64()).unwrap_or(0);
    if minutes == 0 {
        return json_err(400, "偏移量不能为 0");
    }
    if minutes.abs() > 60 * 24 * 366 * 50 {
        return json_err(400, "偏移量超出合理范围");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选择媒体");
    }
    match assets::apply_shift(&conn, &ids, minutes) {
        Ok(n) => {
            let skipped = ids.len() as i64 - n;
            let _ = db::log_op(
                &conn,
                "date-shift",
                None,
                None,
                None,
                None,
                "ok",
                None,
                Some(&format!("偏移 {minutes} 分钟，更新 {n} 项，跳过 {skipped} 项（无日期）")),
            );
            ok(json!({"updated": n, "skipped": skipped, "minutes": minutes}))
        }
        Err(e) => json_err(500, &e),
    }
}

/* ---------------- 相似照片 / Live Photo ---------------- */

fn api_similar(app: &Shared, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let threshold = qs(params, "threshold")
        .and_then(|v| v.parse::<u32>().ok())
        .or_else(|| db::get_setting(&conn, "similar_threshold").and_then(|v| v.parse().ok()))
        .unwrap_or(6)
        .clamp(0, 24);
    match similar::groups_json(&conn, threshold) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn api_similar_hash(app: &Shared) -> Response<Body> {
    let db_path = app.paths.db_path.clone();
    let job = app.jobs.start("phash", "计算相似指纹", 0, move |ctl| {
        if let Err(e) = similar::run_hash(&db_path, &ctl) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({"job_id": job}))
}

fn api_livepairs(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    ok(json!({"pairs": livepair::count(&conn)}))
}

fn api_livepairs_run(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match livepair::run(&conn) {
        Ok((paired, ambiguous)) => {
            let _ = db::log_op(
                &conn,
                "livepair",
                None,
                None,
                None,
                None,
                "ok",
                None,
                Some(&format!("配对 {paired} 组，多对多跳过 {ambiguous} 组")),
            );
            ok(json!({"pairs": paired, "ambiguous": ambiguous}))
        }
        Err(e) => json_err(500, &e),
    }
}

fn api_set_livepair(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let partner = body.get("partner").and_then(|v| v.as_str());
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match livepair::set_pair(&conn, id, partner) {
        Ok(()) => ok(json!({"companion": livepair::companion(&conn, id).unwrap_or(None)})),
        Err(e) => json_err(500, &e),
    }
}

fn api_reveal(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let file_id = body.get("file_id").and_then(|v| v.as_str()).unwrap_or(id);
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let path: Option<String> = conn
        .query_row(
            "SELECT absolute_path FROM media_files WHERE id=?1",
            params![file_id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    match path {
        Some(p) => match util::reveal_in_explorer(Path::new(&p)) {
            Ok(()) => ok(json!(true)),
            Err(e) => json_err(500, &format!("{e}（文件可能已离线）")),
        },
        None => json_err(404, "文件不存在"),
    }
}

fn api_thumb(app: &Shared, id: &str, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let size_arg = qs(params, "size").unwrap_or_else(|| "256".into());
    let (kind, px) = match size_arg.as_str() {
        "640" => ("thumb640", 640u32),
        "poster" => ("poster", 640),
        _ => ("thumb256", 256),
    };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let cached: Option<String> = conn
        .query_row(
            "SELECT cache_path FROM media_previews WHERE asset_id=?1 AND preview_kind=?2",
            params![id, kind],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    if let Some(p) = &cached {
        if let Ok(bytes) = std::fs::read(p) {
            return respond_bytes(
                200,
                bytes,
                "image/jpeg",
                vec![("Cache-Control".into(), "public, max-age=604800".into())],
            );
        }
    }
    if kind == "poster" {
        // 没有缓存时：视频用 FFmpeg 本地抽帧（浏览器解不了的编码也能出封面）
        let Ok(conn2) = open_db(app) else { return json_err(500, "数据库不可用") };
        let file = match assets::primary_file(&conn2, id) {
            Ok(Some(f)) => f,
            _ => return json_err(404, "无可用文件"),
        };
        if util::media_kind(&file.ext) != Some("video") {
            return json_err(404, "暂无封面");
        }
        let dir = app.paths.cache_dir.join("thumb");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{id}-poster.jpg"));
        let src = PathBuf::from(&file.absolute_path);
        // 抽帧时刻取视频 10% 处，太靠前常常是黑场
        let dur_ms: i64 = conn2
            .query_row("SELECT duration_ms FROM media_assets WHERE id=?1", params![id], |r| r.get(0))
            .optional()
            .unwrap_or(None)
            .flatten()
            .unwrap_or(0);
        let at = (dur_ms as f64) / 1000.0 * 0.1;
        match crate::ffmpeg::extract_frame(&src, &path, at.clamp(0.1, 30.0), 640) {
            Ok(_) => {
                let _ = conn2.execute(
                    "INSERT INTO media_previews(asset_id,preview_kind,cache_path,generator_version,updated_at)
                     VALUES(?1,'poster',?2,'1',?3)
                     ON CONFLICT(asset_id,preview_kind) DO UPDATE SET cache_path=excluded.cache_path, updated_at=excluded.updated_at",
                    params![id, path.to_string_lossy(), util::now_iso()],
                );
                let bytes = std::fs::read(&path).unwrap_or_default();
                return respond_bytes(
                    200,
                    bytes,
                    "image/jpeg",
                    vec![("Cache-Control".into(), "public, max-age=604800".into())],
                );
            }
            // 抽不出来不是"请求有问题"，而是"这个视频没有封面"，
            // 回 404 让前端自动回退浏览器抽帧，再不行就显示 ▶ 占位。
            Err(e) => return json_err(404, &format!("抽取视频封面失败：{e}")),
        }
    }
    let Ok(conn2) = open_db(app) else { return json_err(500, "数据库不可用") };
    let file = match assets::primary_file(&conn2, id) {
        Ok(Some(f)) => f,
        _ => return json_err(404, "无可用文件"),
    };
    if util::media_kind(&file.ext) == Some("video") {
        return json_err(404, "视频封面待前端生成");
    }
    if !util::decodable_image(&file.ext) {
        if matches!(file.ext.as_str(), "heic" | "heif") {
            let status = crate::heif::status();
            if !status.ok {
                return json_err(415, &format!("HEIC/HEIF 解码器不可用：{}", status.detail));
            }
        } else {
            return json_err(415, "该格式无内置解码器");
        }
    }
    let src = PathBuf::from(&file.absolute_path);
    let bytes = match media::thumbnail(&src, px) {
        Ok(b) => b,
        Err(e) => return json_err(422, &e),
    };
    let dir = app.paths.cache_dir.join("thumb");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{id}-{px}.jpg"));
    let tmp = dir.join(format!(".{id}-{px}.tmp"));
    if std::fs::write(&tmp, &bytes).is_ok() {
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else {
            let _ = conn2.execute(
                "INSERT INTO media_previews(asset_id,preview_kind,cache_path,generator_version,updated_at)
                 VALUES(?1,?2,?3,'1',?4)
                 ON CONFLICT(asset_id,preview_kind) DO UPDATE SET cache_path=excluded.cache_path, updated_at=excluded.updated_at",
                params![id, kind, path.to_string_lossy(), util::now_iso()],
            );
        }
    }
    respond_bytes(
        200,
        bytes,
        "image/jpeg",
        vec![("Cache-Control".into(), "public, max-age=604800".into())],
    )
}

fn api_poster(app: &Shared, req: &mut Request, id: &str) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let Some(data_url) = body.get("data_url").and_then(|v| v.as_str()) else {
        return json_err(400, "缺少图像数据");
    };
    let payload = data_url.splitn(2, ',').nth(1).unwrap_or(data_url);
    let bytes = match base64::engine::general_purpose::STANDARD.decode(payload) {
        Ok(b) => b,
        Err(_) => return json_err(400, "图像数据无法解码"),
    };
    if bytes.len() < 512 || !bytes.starts_with(&[0xFF, 0xD8]) {
        return json_err(400, "只接受 JPEG 封面");
    }
    let dir = app.paths.cache_dir.join("thumb");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{id}-poster.jpg"));
    if let Err(e) = std::fs::write(&path, &bytes) {
        return json_err(500, &e.to_string());
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let _ = conn.execute(
        "INSERT INTO media_previews(asset_id,preview_kind,cache_path,generator_version,updated_at)
         VALUES(?1,'poster',?2,'1',?3)
         ON CONFLICT(asset_id,preview_kind) DO UPDATE SET cache_path=excluded.cache_path, updated_at=excluded.updated_at",
        params![id, path.to_string_lossy(), util::now_iso()],
    );
    ok(json!({"saved": true}))
}

fn api_image(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500,"数据库不可用") };
    let row: Option<(String,String)> = conn.query_row("SELECT absolute_path,ext FROM media_files WHERE id=?1",params![id],|r|Ok((r.get(0)?,r.get(1)?))).optional().unwrap_or(None);
    let Some((path,ext)) = row else { return json_err(404,"找不到这个文件副本") };
    if util::media_kind(&ext) != Some("image") { return json_err(415,"仅支持图片预览") }
    match media::thumbnail(Path::new(&path),2048) {
        Ok(bytes) => respond_bytes(200,bytes,"image/jpeg",vec![("Cache-Control".into(),"no-store".into())]),
        Err(e) => json_err(422,&e),
    }
}

fn api_file(app: &Shared, req: &Request, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };

    let lookup = |sql: &str| -> Option<(String, String)> {
        conn.query_row(sql, params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .unwrap_or(None)
    };
    // 先按「文件副本 id」找；界面上有些入口传的是条目 id，这里按主文件兜一层，
    // 免得用户点了「用此副本预览」只看到一句「接口不存在」。
    let row = lookup("SELECT absolute_path, ext FROM media_files WHERE id=?1").or_else(|| {
        lookup(
            "SELECT f.absolute_path, f.ext FROM media_assets a \
             JOIN media_files f ON f.asset_id = a.id \
             WHERE a.id = ?1 \
             ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.file_name LIMIT 1",
        )
    });
    let Some((path, ext)) = row else {
        return json_err(404, "找不到这个文件副本（可能已移动或从索引里移除）");
    };
    let Ok(mut file) = std::fs::File::open(&path) else {
        return json_err(410, "源文件当前不可访问（可能未连接）");
    };
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let range = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("range"))
        .map(|h| h.value.as_str().to_string());
    let mime = mime_of(&ext);
    let base_headers = || {
        vec![
            ("Accept-Ranges".to_string(), "bytes".to_string()),
            ("Cache-Control".to_string(), "no-store".to_string()),
        ]
    };
    if let Some(r) = range {
        if let Some(spec) = r.strip_prefix("bytes=") {
            let mut parts = spec.split('-');
            let start_s = parts.next().unwrap_or("");
            let end_s = parts.next().unwrap_or("");
            let start: u64 = start_s.parse().unwrap_or(0);
            let end: u64 = if end_s.is_empty() {
                total.saturating_sub(1)
            } else {
                end_s.parse().unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1))
            };
            if start >= total || start > end {
                return respond_bytes(416, Vec::new(), mime, vec![("Content-Range".into(), format!("bytes */{total}"))]);
            }
            let len = end - start + 1;
            if file.seek(SeekFrom::Start(start)).is_err() {
                return json_err(500, "读取失败");
            }
            let mut headers = base_headers();
            headers.push(("Content-Range".to_string(), format!("bytes {start}-{end}/{total}")));
            let reader = Box::new(file.take(len)) as Body;
            return Response::new(StatusCode(206), {
                let mut hs = vec![header("Content-Type", mime)];
                for (k, v) in headers {
                    hs.push(header(&k, &v));
                }
                hs
            }, reader, Some(len as usize), None);
        }
    }
    let reader = Box::new(file) as Body;
    let mut hs = vec![header("Content-Type", mime)];
    for (k, v) in base_headers() {
        hs.push(header(&k, &v));
    }
    Response::new(StatusCode(200), hs, reader, Some(total as usize), None)
}

fn api_duplicates(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match dedupe::groups_json(&conn) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn api_hash(app: &Shared, req: &mut Request) -> Response<Body> {
    let body = read_body(req).unwrap_or(Value::Null);
    let all = body.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let db_path = app.paths.db_path.clone();
    let job = app.jobs.start("hash", "计算 SHA-256", 0, move |ctl| {
        if let Err(e) = dedupe::run_hash(&db_path, &ctl, all) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({"job_id": job}))
}

fn api_review(app: &Shared, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let mode = qs(params, "mode").unwrap_or_else(|| "all".into());
    let offset: i64 = qs(params, "offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: i64 = qs(params, "limit").and_then(|v| v.parse().ok()).unwrap_or(60).clamp(1, 300);
    let cond = match mode.as_str() {
        "unknown" => "a.effective_taken_at IS NULL".to_string(),
        "low" => "a.effective_taken_at IS NOT NULL AND a.confidence IN ('low','medium')".to_string(),
        "conflict" => "a.effective_taken_at IS NOT NULL AND a.review_reason LIKE '%冲突%'".to_string(),
        _ => "a.needs_review=1".to_string(),
    };
    let total: i64 = conn
        .query_row(&format!("SELECT count(*) FROM media_assets a WHERE {cond}"), [], |r| r.get(0))
        .unwrap_or(0);
    let sql = format!(
        "SELECT a.id,a.media_type,a.effective_taken_at,a.date_source,a.date_precision,a.confidence,
                a.needs_review,a.review_reason,a.width,a.height,a.duration_ms,a.camera,
                a.primary_file_id,a.override_taken_at,
                (SELECT file_name FROM media_files n WHERE n.asset_id=a.id
                 ORDER BY CASE n.availability WHEN 'online' THEN 0 ELSE 1 END, n.rowid LIMIT 1) AS fname
         FROM media_assets a WHERE {cond}
         ORDER BY a.effective_taken_at IS NOT NULL, a.effective_taken_at DESC LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => return json_err(500, &e.to_string()),
    };
    let items: Vec<(String, Value)> = stmt
        .query_map(params![limit, offset], |r| {
            let id: String = r.get(0)?;
            Ok((id, json!({
                "id": r.get::<_, String>(0)?,
                "media_type": r.get::<_, String>(1)?,
                "taken_at": r.get::<_, Option<String>>(2)?,
                "date_source": r.get::<_, Option<String>>(3)?,
                "date_precision": r.get::<_, Option<String>>(4)?,
                "confidence": r.get::<_, String>(5)?,
                "needs_review": r.get::<_, i64>(6)? != 0,
                "review_reason": r.get::<_, Option<String>>(7)?,
                "width": r.get::<_, Option<i64>>(8)?,
                "height": r.get::<_, Option<i64>>(9)?,
                "duration_ms": r.get::<_, Option<i64>>(10)?,
                "camera": r.get::<_, Option<String>>(11)?,
                "file_id": r.get::<_, Option<String>>(12)?,
                "override_taken_at": r.get::<_, Option<String>>(13)?,
                "file_name": r.get::<_, Option<String>>(14)?,
            })))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    let out: Vec<Value> = items
        .into_iter()
        .map(|(id, mut v)| {
            let cands = assets::primary_file(&conn, &id)
                .ok()
                .flatten()
                .map(|f| assets::candidates_of(&f))
                .unwrap_or_default();
            v["candidates"] = serde_json::to_value(cands).unwrap_or(json!([]));
            v
        })
        .collect();
    ok(json!({"total": total, "items": out, "mode": mode}))
}

fn api_plan(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let mut ids: Vec<String> = body
        .get("asset_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        if let Some(f) = body.get("filter") {
            if let Ok(filter) = serde_json::from_value::<query::Filter>(f.clone()) {
                match query::ids(&conn, &filter) {
                    Ok(v) => ids = v,
                    Err(e) => return json_err(500, &e),
                }
            }
        }
    }
    let out_dir = output_dir(app, &body, "archive");
    let template = body.get("template").and_then(|v| v.as_str()).map(|s| s.to_string());
    let conflict = body.get("conflict").and_then(|v| v.as_str()).map(|s| s.to_string());
    let mode = body.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string());
    let request = archive::PlanRequest {
        asset_ids: ids,
        out_dir,
        template,
        conflict,
        mode,
    };
    match archive::create_plan(&conn, &request) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_get_plan(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match archive::get_plan(&conn, id) {
        Ok(v) => ok(v),
        Err(e) => json_err(404, &e),
    }
}

fn api_run_plan(app: &Shared, id: &str) -> Response<Body> {
    let db_path = app.paths.db_path.clone();
    let plan_id = id.to_string();
    let job = app.jobs.start("archive", "复制归档", 0, move |ctl| {
        if let Err(e) = archive::run_plan(&db_path, &ctl, &plan_id) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({"job_id": job}))
}

fn api_plans(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match archive::plans_json(&conn, 40) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn api_undo_plan(app: &Shared, id: &str) -> Response<Body> {
    let db_path = app.paths.db_path.clone();
    let plan_id = id.to_string();
    let job = app.jobs.start("undo", "撤销批次", 0, move |ctl| {
        if let Err(e) = archive::undo_plan(&db_path, &ctl, &plan_id) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({"job_id": job}))
}

fn api_jobs(app: &Shared) -> Response<Body> {
    app.jobs.cleanup();
    let mut list: Vec<Value> = app.jobs.running().iter().map(|j| j.to_json()).collect();
    if let Ok(conn) = open_db(app) {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id,job_type,status,total,done,succeeded,failed,skipped,message,error,created_at,finished_at,
             EXISTS(SELECT 1 FROM processing_outputs po WHERE po.job_id=jobs.id)
             FROM jobs ORDER BY created_at DESC LIMIT 60",
        ) {
            let rows: Vec<Value> = stmt
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "job_type": r.get::<_, String>(1)?,
                        "status": r.get::<_, String>(2)?,
                        "total": r.get::<_, i64>(3)?,
                        "done": r.get::<_, i64>(4)?,
                        "succeeded": r.get::<_, i64>(5)?,
                        "failed": r.get::<_, i64>(6)?,
                        "skipped": r.get::<_, i64>(7)?,
                        "message": r.get::<_, Option<String>>(8)?,
                        "error": r.get::<_, Option<String>>(9)?,
                        "created_at": r.get::<_, String>(10)?,
                        "finished_at": r.get::<_, Option<String>>(11)?,
                        "has_outputs": r.get::<_, bool>(12)?,
                        "persisted": true,
                    }))
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            let running_ids: std::collections::HashSet<String> =
                list.iter().filter_map(|v| v["id"].as_str().map(|s| s.to_string())).collect();
            for row in rows {
                if !running_ids.contains(row["id"].as_str().unwrap_or("")) {
                    list.push(row);
                }
            }
        }
    }
    ok(json!(list))
}

fn api_job_get(app: &Shared, id: &str) -> Response<Body> {
    if let Some(j) = app.jobs.get(id) {
        return ok(j.to_json());
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let row = conn
        .query_row(
            "SELECT id,job_type,status,total,done,succeeded,failed,skipped,message,error,created_at,finished_at
             FROM jobs WHERE id=?1",
            params![id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "job_type": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "total": r.get::<_, i64>(3)?,
                    "done": r.get::<_, i64>(4)?,
                    "succeeded": r.get::<_, i64>(5)?,
                    "failed": r.get::<_, i64>(6)?,
                    "skipped": r.get::<_, i64>(7)?,
                    "message": r.get::<_, Option<String>>(8)?,
                    "error": r.get::<_, Option<String>>(9)?,
                    "created_at": r.get::<_, String>(10)?,
                    "finished_at": r.get::<_, Option<String>>(11)?,
                }))
            },
        )
        .optional()
        .unwrap_or(None);
    match row {
        Some(v) => ok(v),
        None => json_err(404, "任务不存在"),
    }
}

fn api_job_cancel(app: &Shared, id: &str) -> Response<Body> {
    if app.jobs.cancel(id) {
        ok(json!(true))
    } else {
        json_err(404, "任务不在运行中")
    }
}

fn api_logs(app: &Shared, params: &std::collections::HashMap<String, String>) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let limit: i64 = qs(params, "limit").and_then(|v| v.parse().ok()).unwrap_or(100).clamp(1, 2000);
    let mut stmt = match conn.prepare(
        "SELECT id,operation,source_path,target_path,status,timestamp,job_id,detail,before_hash,after_hash
         FROM operation_log ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(e) => return json_err(500, &e.to_string()),
    };
    let rows: Vec<Value> = stmt
        .query_map(params![limit], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "operation": r.get::<_, String>(1)?,
                "source_path": r.get::<_, Option<String>>(2)?,
                "target_path": r.get::<_, Option<String>>(3)?,
                "status": r.get::<_, String>(4)?,
                "timestamp": r.get::<_, String>(5)?,
                "job_id": r.get::<_, Option<String>>(6)?,
                "detail": r.get::<_, Option<String>>(7)?,
                "before_hash": r.get::<_, Option<String>>(8)?,
                "after_hash": r.get::<_, Option<String>>(9)?,
            }))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    ok(json!(rows))
}

fn api_settings_get(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    ok(settings_map(&conn))
}

fn output_root(app: &Shared) -> PathBuf {
    open_db(app).ok().and_then(|conn|db::get_setting(&conn,"output_root"))
        .filter(|s|!s.trim().is_empty()).map(PathBuf::from)
        .unwrap_or_else(||app.paths.data_dir.join("exports"))
}

fn output_dir(app: &Shared, body: &Value, kind: &str) -> String {
    body.get("out_dir").and_then(Value::as_str).filter(|s|!s.trim().is_empty())
        .map(str::to_owned).unwrap_or_else(||output_root(app).join(kind).to_string_lossy().into_owned())
}

fn api_settings_set(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    if let Some(obj) = body.as_object() {
        if let Some(root)=obj.get("output_root").and_then(Value::as_str) {
            if root.trim().is_empty() || !Path::new(root).is_absolute() { return json_err(400,"请选择输出根目录的完整路径") }
            if let Err(e)=std::fs::create_dir_all(root) { return json_err(400,&format!("无法使用输出目录：{e}")) }
        }
        for (k, v) in obj {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if db::set_setting(&conn, k, &value).is_err() {
                return json_err(500, "保存设置失败");
            }
        }
    }
    ok(settings_map(&conn))
}

fn api_backup(app: &Shared) -> Response<Body> {
    let dir = output_root(app).join("backups");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json_err(500, &e.to_string());
    }
    let path = dir.join(format!("library-{}.db", util::stamp()));
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let sql = format!("VACUUM INTO '{}'", path.to_string_lossy().replace('\'', "''"));
    match conn.execute_batch(&sql) {
        Ok(()) => {
            db::log_op(
                &conn,
                "backup",
                None,
                Some(&path.to_string_lossy()),
                None,
                None,
                "ok",
                None,
                None,
            );
            ok(json!({"path": path.to_string_lossy()}))
        }
        Err(e) => json_err(500, &format!("备份失败：{e}")),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn save_export(app: &Shared, bytes: &[u8], ext: &str) -> Response<Body> {
 use std::io::Write;
 let dir=output_root(app).join("manifests"); let path=dir.join(format!("media-{}-{}.{}",util::stamp(),util::uuid(),ext));
 let result=(||->Result<(),String>{std::fs::create_dir_all(&dir).map_err(|e|e.to_string())?; let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e|e.to_string())?; file.write_all(bytes).map_err(|e|e.to_string())})();
 match result {Ok(())=>ok(json!({"path":path.to_string_lossy()})),Err(e)=>json_err(500,&e)}
}
fn api_export(app: &Shared, params: &std::collections::HashMap<String, String>, save: bool) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let filter = filter_from(params);
    let format = qs(params, "format").unwrap_or_else(|| "csv".into());
    let data = match query::list(&conn, &filter, "taken", "desc", 0, 200000) {
        Ok(v) => v,
        Err(e) => return json_err(500, &e),
    };
    let items = data["items"].as_array().cloned().unwrap_or_default();
    if format == "json" {
        // 供未来成长档案网站读取的版本化清单（MVP 不含上传）
        let entries: Vec<Value> = items
            .iter()
            .map(|i| {
                json!({
                    "id": i["id"],
                    "file_name": i["file_name"],
                    "path": i["path"],
                    "media_type": i["media_type"],
                    "taken_at_local": i["taken_at"],
                    "date_source": i["date_source"],
                    "date_precision": i["date_precision"],
                    "confidence": i["confidence"],
                    "sha256": i["sha256"],
                    "size_bytes": i["size_bytes"],
                    "width": i["width"],
                    "height": i["height"],
                    "duration_ms": i["duration_ms"],
                    "camera": i["camera"],
                })
            })
            .collect();
        let manifest = json!({
            "schema": "photo-video-manager/manifest@1",
            "generated_at": util::now_iso(),
            "count": entries.len(),
            "items": entries,
        });
        if save { return save_export(app,&serde_json::to_vec_pretty(&manifest).unwrap_or_default(),"json"); }
        return respond_bytes(
            200,
            serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
            "application/json; charset=utf-8",
            vec![("Content-Disposition".into(), format!("attachment; filename=\"manifest-{}.json\"", util::stamp()))],
        );
    }
    let mut csv = String::from("\u{feff}文件名,拍摄时间,日期来源,置信度,类型,大小(字节),分辨率,时长(毫秒),SHA256,路径\n");
    for i in &items {
        let res = match (i["width"].as_i64(), i["height"].as_i64()) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => String::new(),
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(i["file_name"].as_str().unwrap_or("")),
            csv_escape(i["taken_at"].as_str().unwrap_or("")),
            csv_escape(i["date_source"].as_str().unwrap_or("")),
            csv_escape(i["confidence"].as_str().unwrap_or("")),
            csv_escape(if i["media_type"].as_str() == Some("video") { "视频" } else { "照片" }),
            i["size_bytes"].as_i64().unwrap_or(0),
            res,
            i["duration_ms"].as_i64().unwrap_or(0),
            csv_escape(i["sha256"].as_str().unwrap_or("")),
            csv_escape(i["path"].as_str().unwrap_or("")),
        ));
    }
    if save { return save_export(app,csv.as_bytes(),"csv"); }
    respond_bytes(
        200,
        csv.into_bytes(),
        "text/csv; charset=utf-8",
        vec![("Content-Disposition".into(), format!("attachment; filename=\"media-{}.csv\"", util::stamp()))],
    )
}

// ---------------- 水印 / 网页相册 ----------------

fn api_fonts() -> Response<Body> {
    ok(watermark::font_choices())
}

/// 数据目录下的 fonts\ 目录：用户把字体文件丢进去就能用
fn api_user_fonts(app: &Shared) -> Response<Body> {
    let dir = app.paths.data_dir.join("fonts");
    let _ = std::fs::create_dir_all(&dir);
    let list = watermark::scan_font_dir(&dir);
    ok(json!({ "dir": dir.to_string_lossy(), "fonts": list }))
}

/// 校验自定义字体文件能不能用
fn api_validate_font(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let spec = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    // 用户字体目录里的文件允许只写文件名
    let resolved = if !spec.trim().is_empty() && !PathBuf::from(spec).is_absolute() {
        let d = app.paths.data_dir.join("fonts").join(spec.trim().trim_matches('"'));
        if d.is_file() { d.to_string_lossy().to_string() } else { spec.to_string() }
    } else {
        spec.to_string()
    };
    let r = watermark::validate_custom_font(&resolved);
    if r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        ok(r)
    } else {
        let msg = r.get("error").and_then(|v| v.as_str()).unwrap_or("字体不可用");
        json_err(400, msg)
    }
}

fn api_wm_presets(app: &Shared) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match watermark::presets_list(&conn) {
        Ok(v) => ok(v),
        Err(e) => json_err(500, &e),
    }
}

fn api_wm_preset_save(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = body
        .get("config")
        .cloned()
        .and_then(|v| serde_json::from_value::<watermark::Config>(v).ok())
        .unwrap_or_default();
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match watermark::preset_save(&conn, name, &cfg) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_wm_preset_delete(app: &Shared, id: &str) -> Response<Body> {
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    match watermark::preset_delete(&conn, id) {
        Ok(()) => ok(json!(true)),
        Err(e) => json_err(400, &e),
    }
}

fn api_wm_backups(app: &Shared) -> Response<Body> {
    match watermark::backup_batches(&app.paths.db_path) {
        Ok(v) => ok(json!({ "batches": v })),
        Err(e) => json_err(500, &e),
    }
}

fn api_wm_restore(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let job_id = body.get("job_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if job_id.is_empty() {
        return json_err(400, "缺少批次 ID");
    }
    match watermark::restore_batch(&app.paths.db_path, &job_id) {
        Ok((done, failed, errors)) => ok(json!({
            "restored": done,
            "failed": failed,
            "errors": errors,
        })),
        Err(e) => json_err(400, &e),
    }
}

fn api_wm_drop_backup(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let job_id = body.get("job_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if job_id.is_empty() {
        return json_err(400, "缺少批次 ID");
    }
    match watermark::drop_batch(&app.paths.db_path, &job_id) {
        Ok(n) => ok(json!({ "removed": n })),
        Err(e) => json_err(400, &e),
    }
}

// ---------------- 批量旋转 / 翻转 ----------------

fn api_edit_run(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let op_s = body.get("op").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let action_s = body.get("action").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let action = match crate::imgops::Action::from_params(&op_s, &action_s, &params) {
        Ok(a) => a,
        Err(e) => return json_err(400, &e),
    };
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("copy").to_string();
    let overwrite = mode.eq_ignore_ascii_case("overwrite");
    let out_dir = output_dir(app, &body, "edited");
    if out_dir.is_empty() && !overwrite {
        return json_err(400, "请先选择输出目录");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选中任何照片");
    }
    drop(conn);
    let run = crate::edit::Request {
        ids: ids.clone(),
        action,
        overwrite,
        backup: body.get("backup").and_then(|v| v.as_bool()).unwrap_or(true),
        output_dir: if out_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(out_dir))
        },
        name_template: body
            .get("name_template")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        keep_exif: body.get("keep_exif").and_then(|v| v.as_bool()).unwrap_or(true),
    };
    let data_dir = output_root(app);
    let db_path = app.paths.db_path.clone();
    let total = ids.len() as i64;
    let label = run.action.label();
    let title = if overwrite {
        format!("覆盖原图：{label}")
    } else {
        format!("批量{label}")
    };
    let job = app.jobs.start("edit", &title, total, move |ctl| {
        if let Err(e) = crate::edit::run(&data_dir, &db_path, &ctl, &run) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({ "job_id": job, "count": total, "action": action_s, "op": op_s, "mode": if overwrite { "overwrite" } else { "copy" } }))
}

/// 编辑预览：把动作作用在一张照片上，返回 base64 JPEG，前端实时显示。
fn api_edit_preview(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json_err(400, "缺少照片 id");
    }
    let op_s = body.get("op").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let action_s = body.get("action").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let action = match crate::imgops::Action::from_params(&op_s, &action_s, &params) {
        Ok(a) => a,
        Err(e) => return json_err(400, &e),
    };
    let max_side = body.get("max_side").and_then(|v| v.as_u64()).unwrap_or(1000) as u32;
    match crate::edit::preview(&app.paths.db_path, &id, &action, max_side) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_edit_backups(app: &Shared) -> Response<Body> {
    match crate::edit::backup_batches(&app.paths.db_path) {
        Ok(v) => ok(json!(v)),
        Err(e) => json_err(400, &e),
    }
}

fn api_edit_restore(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let job_id = body.get("job_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if job_id.is_empty() {
        return json_err(400, "缺少批次 ID");
    }
    match crate::edit::restore_batch(&app.paths.db_path, &job_id) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_edit_drop(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let job_id = body.get("job_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if job_id.is_empty() {
        return json_err(400, "缺少批次 ID");
    }
    match crate::edit::drop_batch(&app.paths.db_path, &job_id) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_wm_mark(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body)=read_body(req) else { return json_err(400,"请求格式错误") };
    let Ok(conn)=open_db(app) else { return json_err(500,"数据库不可用") };
    let marked=body.get("marked").and_then(Value::as_bool).unwrap_or(true);
    let ids=match ids_from_body(&conn,&body) {Ok(v)=>v,Err(e)=>return json_err(400,&e)};
    for id in &ids {
        let file=match assets::primary_file(&conn,id) {Ok(Some(f))=>f,_=>return json_err(400,"照片文件不可用")};
        if let Err(e)=watermark::mark_path(&conn,Path::new(&file.absolute_path),marked) {return json_err(400,&e)}
    }
    ok(json!({"updated":ids.len()}))
}

fn api_wm_rotation(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body)=read_body(req) else { return json_err(400,"请求格式错误") };
    let id=body.get("id").and_then(Value::as_str).unwrap_or("");
    let Some(degrees)=body.get("degrees").and_then(Value::as_i64).filter(|v|[0,90,180,270].contains(v)) else { return json_err(400,"方向必须为 0、90、180 或 270") };
    let Ok(conn)=open_db(app) else { return json_err(500,"数据库不可用") };
    match conn.execute("INSERT OR REPLACE INTO watermark_rotation(asset_id,degrees) VALUES(?1,?2)",params![id,degrees]) {
        Ok(_)=>ok(json!(true)),Err(e)=>json_err(400,&e.to_string())
    }
}

fn api_wm_preview(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return json_err(400, "缺少照片 id");
    }
    let cfg = body
        .get("config")
        .cloned()
        .and_then(|v| serde_json::from_value::<watermark::Config>(v).ok())
        .unwrap_or_default();
    let max_side = body.get("max_side").and_then(|v| v.as_u64()).unwrap_or(1400) as u32;
    match watermark::preview(&app.paths.db_path, id, &cfg, max_side) {
        Ok(v) => ok(v),
        Err(e) => json_err(400, &e),
    }
}

fn api_wm_run(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let mode = body
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("copy")
        .to_string();
    let overwrite = mode.eq_ignore_ascii_case("overwrite");
    let out_dir = output_dir(app, &body, "watermark");
    if out_dir.is_empty() && !overwrite {
        return json_err(400, "请先选择输出目录");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选中任何照片");
    }
    let defaults = watermark::RunRequest::default();
    let config = body
        .get("config")
        .cloned()
        .and_then(|v| serde_json::from_value::<watermark::Config>(v).ok())
        .unwrap_or_default();
    let run = watermark::RunRequest {
        backup_dir: output_root(app).join("backups").to_string_lossy().into_owned(),
        ids: ids.clone(),
        out_dir,
        config,
        name_template: body
            .get("name_template")
            .and_then(|v| v.as_str())
            .unwrap_or(&defaults.name_template)
            .to_string(),
        quality: body.get("quality").and_then(|v| v.as_u64()).unwrap_or(92) as u8,
        keep_exif: body.get("keep_exif").and_then(|v| v.as_bool()).unwrap_or(true),
        skip_existing: body.get("skip_existing").and_then(|v| v.as_bool()).unwrap_or(true),
        mode: if overwrite { "overwrite".into() } else { "copy".into() },
        backup: body.get("backup").and_then(|v| v.as_bool()).unwrap_or(true),
    };
    drop(conn);
    let db_path = app.paths.db_path.clone();
    let total = ids.len() as i64;
    let title = if overwrite { "覆盖原图打水印" } else { "批量生成时间水印" };
    let job = app.jobs.start("watermark", title, total, move |ctl| {
        if let Err(e) = watermark::run(&db_path, &ctl, run) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({ "job_id": job, "count": total, "mode": if overwrite { "overwrite" } else { "copy" } }))
}

fn api_album_run(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let out_dir = output_dir(app, &body, "albums");
    if out_dir.is_empty() {
        return json_err(400, "请先选择输出目录");
    }
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选中任何照片");
    }
    let defaults = album::AlbumRequest::default();
    let run = album::AlbumRequest {
        ids: ids.clone(),
        out_dir,
        title: body
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(&defaults.title)
            .to_string(),
        max_side: body.get("max_side").and_then(|v| v.as_u64()).unwrap_or(1600) as u32,
        thumb_side: body.get("thumb_side").and_then(|v| v.as_u64()).unwrap_or(480) as u32,
        include_originals: body
            .get("include_originals")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        quality: body.get("quality").and_then(|v| v.as_u64()).unwrap_or(84) as u8,
        zip: body.get("zip").and_then(|v| v.as_bool()).unwrap_or(false),
        password: body
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        theme: body
            .get("theme")
            .and_then(|v| v.as_str())
            .unwrap_or(&defaults.theme)
            .to_string(),
        watermark: match body.get("watermark").filter(|v| !v.is_null()) {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(cfg) => Some(cfg),
                Err(e) => return json_err(400,&format!("水印设置不正确：{e}")),
            },
            None => None,
        },
    };
    drop(conn);
    let db_path = app.paths.db_path.clone();
    let total = ids.len() as i64;
    let job = app.jobs.start("album", "导出网页相册", total, move |ctl| {
        if let Err(e) = album::run(&db_path, &ctl, run) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({ "job_id": job, "count": total }))
}

fn api_slideshow_run(app: &Shared, req: &mut Request) -> Response<Body> {
    let Some(body) = read_body(req) else { return json_err(400, "请求格式错误") };
    let out_dir = output_dir(app, &body, "slideshows");
    if out_dir.is_empty() {
        return json_err(400, "请先选择输出目录");
    }
    let fmt_s = body.get("format").and_then(|v| v.as_str()).unwrap_or("mp4").to_string();
    let Some(format) = crate::slideshow::Format::parse(&fmt_s) else {
        return json_err(400, &format!("不支持的格式：{fmt_s}"));
    };
    let Ok(conn) = open_db(app) else { return json_err(500, "数据库不可用") };
    let ids = match ids_from_body(&conn, &body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e),
    };
    if ids.is_empty() {
        return json_err(400, "没有选中任何素材");
    }
    drop(conn);
    let defaults = crate::slideshow::Request::default();
    let run = crate::slideshow::Request {
        ids: ids.clone(),
        out_dir,
        file_name: body
            .get("file_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&defaults.file_name)
            .to_string(),
        format,
        photo_seconds: body
            .get("photo_seconds")
            .and_then(|v| v.as_f64())
            .map(|v| v.clamp(0.5, 60.0) as f32)
            .unwrap_or(defaults.photo_seconds),
        video_seconds: body
            .get("video_seconds")
            .and_then(|v| v.as_f64())
            .map(|v| v.clamp(0.0, 120.0) as f32)
            .unwrap_or(defaults.video_seconds),
        size: body.get("size").and_then(|v| v.as_u64()).unwrap_or(720) as u32,
        background: body
            .get("background")
            .and_then(|v| v.as_str())
            .unwrap_or(&defaults.background)
            .to_string(),
    };
    let db_path = app.paths.db_path.clone();
    let total = ids.len() as i64;
    let label = format.label().to_string();
    let job = app.jobs.start("slideshow", &format!("合成{label}"), total, move |ctl| {
        if let Err(e) = crate::slideshow::run(&db_path, &ctl, &run) {
            ctl.finish(Some(e));
        }
    });
    ok(json!({ "job_id": job, "count": total, "format": fmt_s }))
}
