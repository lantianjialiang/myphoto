//! 只读扫描与索引：发现文件 → 提取元数据 → 写库。绝不修改原件。

use crate::assets;
use crate::dateinfer;
use crate::db;
use crate::jobs::JobCtl;
use crate::media;
use crate::util;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SourceOptions {
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub ext_excludes: Vec<String>,
}

struct Found {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
    ext: String,
    rel_dir: String,
}

fn dir_excluded(dir: &Path, opts: &SourceOptions, data_dir: &Path) -> bool {
    if util::path_is_inside(dir, data_dir) {
        return true;
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.eq_ignore_ascii_case("$RECYCLE.BIN") || name.eq_ignore_ascii_case("System Volume Information") {
        return true;
    }
    for ex in &opts.excludes {
        let ex = ex.trim();
        if ex.is_empty() {
            continue;
        }
        let p = Path::new(ex);
        if p.is_absolute() {
            if util::path_is_inside(dir, p) {
                return true;
            }
        } else if name.eq_ignore_ascii_case(ex) {
            return true;
        }
    }
    false
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 执行一次扫描（后台任务体）。
pub fn run(db_path: &Path, job: &JobCtl, source_id: &str, data_dir: &Path) -> Result<(), String> {
    let conn = db::open(db_path)?;
    let row = conn
        .query_row(
            "SELECT root_path, options_json FROM sources WHERE id=?1",
            params![source_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "媒体源不存在".to_string())?;
    let (root, options_json) = row;
    let opts: SourceOptions = serde_json::from_str(&options_json).unwrap_or_default();
    let root_path = PathBuf::from(&root);

    if !root_path.is_dir() {
        let _ = conn.execute(
            "UPDATE sources SET is_online=0 WHERE id=?1",
            params![source_id],
        );
        db::log_op(
            &conn,
            "scan",
            Some(&root),
            None,
            None,
            None,
            "offline",
            Some(&job.id),
            Some("源离线或不可访问；索引保留，不判定为删除"),
        );
        return Err(format!("媒体源离线或不可访问：{root}"));
    }
    if util::path_is_inside(&root_path, data_dir) {
        return Err("媒体源不能包含应用数据目录".into());
    }

    job.phase("正在发现文件…");
    let mut entries: Vec<Found> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut skipped_ext: i64 = 0;

    let walker = WalkDir::new(&root_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if !e.file_type().is_dir() {
                return true;
            }
            !dir_excluded(e.path(), &opts, data_dir)
        });
    for item in walker {
        if job.is_cancelled() {
            break;
        }
        let entry = match item {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("目录读取失败：{e}"));
                continue;
            }
        };
        if entry.file_type().is_dir() {
            continue;
        }
        if entry.file_type().is_symlink() {
            continue; // 默认不追踪符号链接/目录联接，避免循环与越界
        }
        let path = entry.path().to_path_buf();
        let ext = util::ext_of(&path);
        if util::media_kind(&ext).is_none() {
            continue;
        }
        if opts
            .ext_excludes
            .iter()
            .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case(&ext))
        {
            skipped_ext += 1;
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{}：{e}", path.display()));
                continue;
            }
        };
        let rel_dir = path
            .parent()
            .and_then(|p| p.strip_prefix(&root_path).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        entries.push(Found {
            size: meta.len(),
            mtime_ms: mtime_ms(&meta),
            ext,
            rel_dir,
            path,
        });
        job.set_total(entries.len() as i64);
        if entries.len() % 200 == 0 {
            job.phase(&format!("正在发现文件… 已发现 {} 个", entries.len()));
        }
    }
    if job.is_cancelled() {
        job.phase("已取消（未做任何写入）");
        return Ok(());
    }

    job.phase(&format!("正在读取元数据（{} 个文件）…", entries.len()));
    let scan_id = util::uuid();
    let total = entries.len() as i64;
    job.set_total(total);
    let mut added = 0i64;
    let mut updated = 0i64;
    let mut unchanged = 0i64;
    let mut failed = 0i64;

    db::begin_write(&conn)?;
    // 事务不能握太久：写事务期间其他连接（界面上的标签、星标、日志）要排队等锁。
    // 每 50 个文件或每 700ms 提交一次，把等待上限压到 1 秒内。
    let mut tx_started = std::time::Instant::now();
    for (idx, f) in entries.iter().enumerate() {
        if job.is_cancelled() {
            break;
        }
        // 单个文件是一次写入单位：遇到锁冲突就回滚重开事务重试，别让整批扫描全废。
        let mut attempt = 0u32;
        let outcome = loop {
            match index_file(&conn, source_id, f, &scan_id, &root_path) {
                Err(e) if db::is_busy_err(&e) && attempt < 3 => {
                    attempt += 1;
                    let _ = conn.execute_batch("ROLLBACK");
                    db::begin_write(&conn)?;
                    tx_started = std::time::Instant::now();
                }
                other => break other,
            }
        };
        match outcome {
            Ok(State::Added) => added += 1,
            Ok(State::Updated) => updated += 1,
            Ok(State::Unchanged) => unchanged += 1,
            Err(e) => {
                failed += 1;
                if errors.len() < 300 {
                    errors.push(format!("{}：{e}", f.path.display()));
                }
            }
        }
        job.ok();
        if idx > 0 && (idx % 50 == 0 || tx_started.elapsed().as_millis() > 700) {
            job.phase(&format!(
                "正在读取元数据… {}/{}",
                idx + 1,
                total
            ));
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            db::begin_write(&conn)?;
            tx_started = std::time::Instant::now();
        }
    }
    let cancelled = job.is_cancelled();
    if !cancelled {
        // 只有完整扫描后才能判定未出现的文件为“缺失”；离线源不受影响。
        // 只处理仍位于本源根目录下的记录：被「移动」功能搬到别处的文件不会误判为缺失。
        let _ = conn.execute(
            "UPDATE media_files SET availability='missing'
             WHERE source_id=?1 AND (last_seen_scan_id IS NULL OR last_seen_scan_id<>?2)
               AND lower(absolute_path) LIKE ?3 ESCAPE '\\'",
            params![source_id, scan_id, like_inside(&root)],
        );
    }
    let _ = conn.execute(
        "UPDATE sources SET is_online=1, last_scan_at=?2 WHERE id=?1",
        params![source_id, util::now_iso()],
    );
    if let Err(e) = conn.execute_batch("COMMIT") {
        let msg = format!("提交扫描事务失败：{e}（本次读取的元数据可能未保存）");
        let _ = db::log_op(
            &conn,
            "scan-error",
            Some(&root),
            None,
            None,
            None,
            "error",
            Some(&job.id),
            Some(&msg),
        );
        return Err(msg);
    }

    let detail = format!(
        "新增 {}，更新 {}，未变 {}，失败 {}，跳过扩展名 {}，错误 {}",
        added,
        updated,
        unchanged,
        failed,
        skipped_ext,
        errors.len()
    );
    let detail = match errors.first() {
        Some(first) => format!("{detail}；首个问题：{first}"),
        None => detail,
    };
    db::log_op(
        &conn,
        "scan",
        Some(&root),
        None,
        None,
        None,
        if cancelled { "cancelled" } else if failed > 0 || !errors.is_empty() { "partial" } else { "ok" },
        Some(&job.id),
        Some(&detail),
    );
    for err in errors.iter().take(100) {
        db::log_op(
            &conn,
            "scan-error",
            Some(err),
            None,
            None,
            None,
            "error",
            Some(&job.id),
            None,
        );
    }
    job.phase(&detail);
    Ok(())
}

enum State {
    Added,
    Updated,
    Unchanged,
}

/// 把 SQLite 错误连同扩展码一起带出来：锁冲突要能区分 BUSY / READONLY / IOERR，
/// 否则日志里只剩一句 "database is locked"，无法定位是谁拿着锁。
fn sqlite_err(e: rusqlite::Error) -> String {
    match &e {
        rusqlite::Error::SqliteFailure(f, msg) => {
            let extra = msg.as_deref().unwrap_or("");
            format!("{e}（{:?}/{:?}{}）", f.code, f.extended_code, if extra.is_empty() { String::new() } else { format!(" {extra}") })
        }
        _ => e.to_string(),
    }
}

fn index_file(conn: &Connection, source_id: &str, f: &Found, scan_id: &str, root: &Path) -> Result<State, String> {
    let file_name = util::file_name_of(&f.path);
    let existing = conn
        .query_row(
            "SELECT id, asset_id, size_bytes, mtime_ms, taken_at FROM media_files
             WHERE source_id=?1 AND absolute_path=?2",
            params![source_id, f.path.to_string_lossy()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_err)?;

    if let Some((file_id, asset_id, size, mt, taken)) = existing {
        let unchanged = size == f.size as i64 && mt == f.mtime_ms && taken.is_some();
        if unchanged {
            conn.execute(
                "UPDATE media_files SET availability='online', last_seen_scan_id=?2 WHERE id=?1",
                params![file_id, scan_id],
            )
            .map_err(sqlite_err)?;
            return Ok(State::Unchanged);
        }
        let dirs = dir_chain(&f.rel_dir);
        let ex = media::extract(&f.path, &f.ext, util::media_kind(&f.ext).unwrap_or("image"), &dirs, f.mtime_ms);
        write_metadata(conn, &file_id, f, &ex)?;
        invalidate_previews(conn, &asset_id)?;
        assets::recompute(conn, &asset_id)?;
        return Ok(State::Updated);
    }

    let asset_id = util::uuid();
    let file_id = util::uuid();
    let now = util::now_iso();
    let kind = util::media_kind(&f.ext).unwrap_or("image");
    conn.execute(
        "INSERT INTO media_assets(id, media_type, created_at) VALUES(?1,?2,?3)",
        params![asset_id, kind, now],
    )
    .map_err(sqlite_err)?;
    conn.execute(
        "INSERT INTO media_files(id,asset_id,source_id,absolute_path,rel_dir,file_name,ext,size_bytes,
         mtime_ms,availability,last_seen_scan_id,added_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'online',?10,?11)",
        params![
            file_id,
            asset_id,
            source_id,
            f.path.to_string_lossy(),
            f.rel_dir,
            file_name,
            f.ext,
            f.size as i64,
            f.mtime_ms,
            scan_id,
            now
        ],
    )
    .map_err(sqlite_err)?;
    let dirs = dir_chain(&f.rel_dir);
    let ex = media::extract(&f.path, &f.ext, kind, &dirs, f.mtime_ms);
    write_metadata(conn, &file_id, f, &ex)?;
    assets::recompute(conn, &asset_id)?;
    let _ = root;
    Ok(State::Added)
}

/// 构造“位于该目录之下”的 LIKE 模式（供 SQL 的 ESCAPE '\' 使用）。
fn like_inside(root: &str) -> String {
    let root = root.replace('/', "\\");
    let root = root.trim_end_matches('\\');
    let mut out = String::with_capacity(root.len() + 3);
    for c in root.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str("\\\\%");
    out.to_lowercase()
}

/// 目录链按“由近到远”排列，供目录名日期推断使用。
fn dir_chain(rel_dir: &str) -> Vec<String> {
    let mut v: Vec<String> = rel_dir
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    v.reverse();
    v
}

fn write_metadata(conn: &Connection, file_id: &str, f: &Found, ex: &media::Extracted) -> Result<(), String> {
    let sel = dateinfer::select(&ex.candidates, None);
    let meta = serde_json::json!({
        "width": ex.width,
        "height": ex.height,
        "duration_ms": ex.duration_ms,
        "camera": ex.camera,
        "gps_lat": ex.gps.map(|g| g.0),
        "gps_lon": ex.gps.map(|g| g.1),
        "error": ex.error,
        "details": ex.details,
    });
    conn.execute(
        "UPDATE media_files SET size_bytes=?2, mtime_ms=?3, sha256=NULL, metadata_json=?4,
         candidates_json=?5, taken_at=?6, date_source=?7, date_confidence=?8 WHERE id=?1",
        params![
            file_id,
            f.size as i64,
            f.mtime_ms,
            meta.to_string(),
            serde_json::to_string(&ex.candidates).unwrap_or_else(|_| "[]".into()),
            sel.effective,
            sel.source,
            sel.confidence
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn found_file(path: &Path, root: &Path) -> Result<Found, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    Ok(Found {
        path: path.to_path_buf(), size: meta.len(), mtime_ms: mtime_ms(&meta),
        ext: path.extension().unwrap_or_default().to_string_lossy().to_lowercase(),
        rel_dir: path.parent().and_then(|p| p.strip_prefix(root).ok()).unwrap_or(Path::new("")).to_string_lossy().into_owned(),
    })
}

fn invalidate_previews(conn: &Connection, id: &str) -> Result<(), String> {
    let mut st = conn.prepare("SELECT cache_path FROM media_previews WHERE asset_id=?1").map_err(sqlite_err)?;
    let paths = st.query_map(params![id], |r| r.get::<_, String>(0)).map_err(sqlite_err)?;
    for p in paths.flatten() { let _ = std::fs::remove_file(p); }
    conn.execute("DELETE FROM media_previews WHERE asset_id=?1", params![id]).map_err(sqlite_err)?;
    conn.execute("UPDATE media_assets SET phash=NULL WHERE id=?1", params![id]).map_err(sqlite_err)?;
    Ok(())
}

/// 覆盖和还原共用：重读实际文件，并把已不相同的重复副本拆成独立资产。
pub fn refresh_modified(conn: &Connection, path: &Path) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(sqlite_err)?;
    let rows: Vec<(String, String)> = {
        let mut st = tx.prepare("SELECT id,asset_id FROM media_files WHERE absolute_path=?1 COLLATE NOCASE").map_err(sqlite_err)?;
        let rows = st.query_map(params![path.to_string_lossy()], |r| Ok((r.get(0)?,r.get(1)?))).map_err(sqlite_err)?;
        rows.collect::<Result<_,_>>().map_err(sqlite_err)?
    };
    let f = found_file(path, path.parent().unwrap_or(Path::new("")))?;
    let ex = media::extract(path, &f.ext, util::media_kind(&f.ext).unwrap_or("image"), &[], f.mtime_ms);
    for (file_id, old_asset) in rows {
        let count: i64 = tx.query_row("SELECT count(*) FROM media_files WHERE asset_id=?1", params![old_asset], |r| r.get(0)).map_err(sqlite_err)?;
        let asset = if count > 1 {
            let new_id = util::uuid();
            tx.execute("INSERT INTO media_assets(id,media_type,override_taken_at,star,favorite,created_at)
                SELECT ?1,media_type,override_taken_at,star,favorite,?2 FROM media_assets WHERE id=?3",
                params![new_id,util::now_iso(),old_asset]).map_err(sqlite_err)?;
            tx.execute("INSERT INTO asset_tags(asset_id,tag_id,created_at) SELECT ?1,tag_id,created_at FROM asset_tags WHERE asset_id=?2",params![new_id,old_asset]).map_err(sqlite_err)?;
            tx.execute("UPDATE media_files SET asset_id=?1 WHERE id=?2",params![new_id,file_id]).map_err(sqlite_err)?;
            invalidate_previews(&tx,&old_asset)?;
            assets::recompute(&tx,&old_asset)?;
            new_id
        } else { old_asset };
        write_metadata(&tx,&file_id,&f,&ex)?;
        invalidate_previews(&tx,&asset)?;
        assets::recompute(&tx,&asset)?;
    }
    tx.commit().map_err(sqlite_err)
}

/// 处理后的副本立即入库，并记录来源和任务，供下一步导出相册。
pub fn register_output(conn: &Connection, source_asset: &str, job_id: &str, kind: &str, path: &Path) -> Result<(), String> {
    let path = util::clean_path(&std::fs::canonicalize(path).map_err(|e| e.to_string())?);
    let parent = path.parent().ok_or("输出路径没有父目录")?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(sqlite_err)?;
    let sources: Vec<(String,String)> = {
        let mut st = tx.prepare("SELECT id,root_path FROM sources ORDER BY length(root_path) DESC").map_err(sqlite_err)?;
        let rows = st.query_map([],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sqlite_err)?;
        rows.collect::<Result<_,_>>().map_err(sqlite_err)?
    };
    let existing = sources.into_iter().find(|(_,root)| util::path_is_inside(&path,Path::new(root)));
    let (source_id,root) = match existing {
        Some((id,root)) => (id,PathBuf::from(root)),
        None => {
            let id = util::uuid();
            tx.execute("INSERT INTO sources(id,root_path,label,created_at) VALUES(?1,?2,?3,?4)",params![id,parent.to_string_lossy(),"处理结果",util::now_iso()]).map_err(sqlite_err)?;
            (id,parent.to_path_buf())
        }
    };
    let f = found_file(&path,&root)?;
    index_file(&tx,&source_id,&f,job_id,&root)?;
    let (file_id,asset_id): (String,String) = tx.query_row("SELECT id,asset_id FROM media_files WHERE source_id=?1 AND absolute_path=?2",params![source_id,path.to_string_lossy()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sqlite_err)?;
    tx.execute("UPDATE media_assets SET override_taken_at=(SELECT effective_taken_at FROM media_assets WHERE id=?1) WHERE id=?2",params![source_asset,asset_id]).map_err(sqlite_err)?;
    assets::recompute(&tx,&asset_id)?;
    tx.execute("INSERT OR REPLACE INTO processing_outputs(output_file_id,source_asset_id,job_id,kind) VALUES(?1,?2,?3,?4)",params![file_id,source_asset,job_id,kind]).map_err(sqlite_err)?;
    tx.commit().map_err(sqlite_err)
}
