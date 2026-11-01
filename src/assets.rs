//! 逻辑资产（media_assets）与文件实体（media_files）的重算与查询。
//!
//! 一个资产可以对应多个路径（同一内容在不同文件夹/盘符），路径不是主键。

use crate::dateinfer::{self, Candidate};
use crate::db::R;
use crate::util;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct FileRow {
    pub id: String,
    pub asset_id: String,
    pub source_id: String,
    pub absolute_path: String,
    pub rel_dir: Option<String>,
    pub file_name: String,
    pub ext: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
    pub sha256: Option<String>,
    pub availability: String,
    pub taken_at: Option<String>,
    pub date_source: Option<String>,
    pub date_confidence: Option<String>,
    pub candidates_json: Option<String>,
    pub metadata_json: Option<String>,
    pub marked: i64,
}

const FILE_COLS: &str = "f.id,f.asset_id,f.source_id,f.absolute_path,f.rel_dir,f.file_name,f.ext,\
f.size_bytes,f.mtime_ms,f.sha256,f.availability,f.taken_at,f.date_source,f.date_confidence,\
f.candidates_json,f.metadata_json,f.marked";

fn map_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        asset_id: r.get(1)?,
        source_id: r.get(2)?,
        absolute_path: r.get(3)?,
        rel_dir: r.get(4)?,
        file_name: r.get(5)?,
        ext: r.get(6)?,
        size_bytes: r.get(7)?,
        mtime_ms: r.get(8)?,
        sha256: r.get(9)?,
        availability: r.get(10)?,
        taken_at: r.get(11)?,
        date_source: r.get(12)?,
        date_confidence: r.get(13)?,
        candidates_json: r.get(14)?,
        metadata_json: r.get(15)?,
        marked: r.get(16)?,
    })
}

pub fn files_of_asset(conn: &Connection, asset_id: &str) -> R<Vec<FileRow>> {
    let sql = format!(
        "SELECT {FILE_COLS} FROM media_files f WHERE f.asset_id=?1
         ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.rowid"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![asset_id], |r| map_file(r))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn primary_file(conn: &Connection, asset_id: &str) -> R<Option<FileRow>> {
    Ok(files_of_asset(conn, asset_id)?.into_iter().next())
}

pub fn candidates_of(file: &FileRow) -> Vec<Candidate> {
    file.candidates_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<Candidate>>(s).ok())
        .unwrap_or_default()
}

/// 汇总资产下所有文件的日期候选，重新计算有效日期与是否需要人工确认。
pub fn recompute(conn: &Connection, asset_id: &str) -> R<()> {
    let override_v: Option<String> = conn
        .query_row(
            "SELECT override_taken_at FROM media_assets WHERE id=?1",
            params![asset_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let files = files_of_asset(conn, asset_id)?;
    let mut cands: Vec<Candidate> = Vec::new();
    for f in &files {
        cands.extend(candidates_of(f));
    }
    // 同一资产下同一来源只保留最具体的一条，避免重复干扰
    cands.sort_by_key(|c| -(dateinfer::source_rank(&c.source) as i64));
    let sel = dateinfer::select(&cands, override_v.as_deref());

    let primary = files
        .iter()
        .find(|f| f.availability == "online")
        .or_else(|| files.first());
    let meta: serde_json::Value = primary
        .and_then(|f| f.metadata_json.as_deref())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::json!({}));
    let width = meta.get("width").and_then(|v| v.as_i64());
    let height = meta.get("height").and_then(|v| v.as_i64());
    let duration = meta.get("duration_ms").and_then(|v| v.as_i64());
    let camera = meta.get("camera").and_then(|v| v.as_str()).map(|s| s.to_string());
    let gps_lat = meta.get("gps_lat").and_then(|v| v.as_f64());
    let gps_lon = meta.get("gps_lon").and_then(|v| v.as_f64());
    let media_type = if primary.map(|f| util::media_kind(&f.ext) == Some("video")).unwrap_or(false) {
        "video"
    } else {
        "image"
    };

    conn.execute(
        "UPDATE media_assets SET media_type=?2, effective_taken_at=?3, date_source=?4, date_precision=?5,
         confidence=?6, needs_review=?7, review_reason=?8, primary_file_id=?9, width=?10, height=?11,
         duration_ms=?12, camera=?13, gps_lat=?14, gps_lon=?15 WHERE id=?1",
        params![
            asset_id,
            media_type,
            sel.effective,
            sel.source,
            sel.precision,
            sel.confidence,
            sel.needs_review as i64,
            sel.reason,
            primary.map(|f| f.id.clone()),
            width,
            height,
            duration,
            camera,
            gps_lat,
            gps_lon
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 资产级 JSON（含文件列表），用于详情与列表。
pub fn asset_json(conn: &Connection, asset_id: &str, with_files: bool) -> R<Option<serde_json::Value>> {
    let row = conn
        .query_row(
            "SELECT a.id,a.media_type,a.effective_taken_at,a.date_source,a.date_precision,a.confidence,
             a.override_taken_at,a.needs_review,a.review_reason,a.width,a.height,a.duration_ms,a.camera,
             a.gps_lat,a.gps_lon,a.metadata_json,a.primary_file_id,a.star,a.favorite,a.phash,a.live_pair_id
             FROM media_assets a WHERE a.id=?1",
            params![asset_id],
            |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "media_type": r.get::<_, String>(1)?,
                    "taken_at": r.get::<_, Option<String>>(2)?,
                    "date_source": r.get::<_, Option<String>>(3)?,
                    "date_source_label": crate::dateinfer::label(&r.get::<_, Option<String>>(3)?.unwrap_or_default()),
                    "date_precision": r.get::<_, Option<String>>(4)?,
                    "confidence": r.get::<_, String>(5)?,
                    "override_taken_at": r.get::<_, Option<String>>(6)?,
                    "needs_review": r.get::<_, i64>(7)? != 0,
                    "review_reason": r.get::<_, Option<String>>(8)?,
                    "width": r.get::<_, Option<i64>>(9)?,
                    "height": r.get::<_, Option<i64>>(10)?,
                    "duration_ms": r.get::<_, Option<i64>>(11)?,
                    "camera": r.get::<_, Option<String>>(12)?,
                    "gps_lat": r.get::<_, Option<f64>>(13)?,
                    "gps_lon": r.get::<_, Option<f64>>(14)?,
                    "metadata_json": r.get::<_, Option<String>>(15)?,
                    "primary_file_id": r.get::<_, Option<String>>(16)?,
                    "star": r.get::<_, i64>(17)? != 0,
                    "favorite": r.get::<_, i64>(18)? != 0,
                    "phash": r.get::<_, Option<String>>(19)?,
                    "live_pair_id": r.get::<_, Option<String>>(20)?,
                }))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(mut value) = row else { return Ok(None) };
    let files = files_of_asset(conn, asset_id)?;
    value["file_count"] = serde_json::json!(files.len());
    value["tags"] = serde_json::to_value(crate::tags::of_asset(conn, asset_id)?)
        .unwrap_or(serde_json::json!([]));
    if with_files {
        value["files"] = serde_json::to_value(&files).unwrap_or(serde_json::json!([]));
    }
    Ok(Some(value))
}

/// 合并同内容资产：把 `from` 下的文件全部挂到 `into`，并清理空资产。
/// 标签、星标、收藏、指纹等资产级信息一并继承，避免合并丢数据。
pub fn merge_into(conn: &Connection, into: &str, from: &str) -> R<()> {
    if into == from {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO asset_tags(asset_id,tag_id,created_at)
         SELECT ?1, tag_id, created_at FROM asset_tags WHERE asset_id=?2",
        params![into, from],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM asset_tags WHERE asset_id=?1", params![from])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE media_assets SET
           star = MAX(star, COALESCE((SELECT star FROM media_assets WHERE id=?2),0)),
           favorite = MAX(favorite, COALESCE((SELECT favorite FROM media_assets WHERE id=?2),0)),
           phash = COALESCE(phash, (SELECT phash FROM media_assets WHERE id=?2))
         WHERE id=?1",
        params![into, from],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE media_files SET asset_id=?1 WHERE asset_id=?2",
        params![into, from],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM media_previews WHERE asset_id=?1", params![from])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM media_assets WHERE id=?1", params![from])
        .map_err(|e| e.to_string())?;
    recompute(conn, into)?;
    Ok(())
}

/// 批量时间偏移：以当前显示日期为基准平移，结果写入人工覆盖值（原始 EXIF 不变）。
/// 返回实际修改的资产数；没有日期的资产会被跳过，绝不猜测。
pub fn apply_shift(conn: &Connection, ids: &[String], minutes: i64) -> R<i64> {
    if minutes == 0 {
        return Ok(0);
    }
    let mut n = 0i64;
    for id in ids {
        let base: Option<String> = conn
            .query_row(
                "SELECT effective_taken_at FROM media_assets WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let Some(base) = base else { continue };
        let Some(dt) = crate::dateinfer::parse_iso(&base) else { continue };
        let shifted = dt + chrono::Duration::minutes(minutes);
        let value = crate::dateinfer::iso(shifted);
        conn.execute(
            "UPDATE media_assets SET override_taken_at=?2 WHERE id=?1",
            params![id, value],
        )
        .map_err(|e| e.to_string())?;
        recompute(conn, id)?;
        n += 1;
    }
    Ok(n)
}

/// 文件被移动后同步索引：更新路径、文件名与相对目录，并在目标落在某个媒体源内时
/// 自动挂到那个源（否则保留原 source_id，由扫描的“缺失”判定跳过它）。
pub fn relocate_file(conn: &Connection, file_id: &str, new_path: &str) -> R<()> {
    let (asset_id, source_id): (String, String) = conn
        .query_row(
            "SELECT asset_id, source_id FROM media_files WHERE id=?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let p = std::path::Path::new(new_path);
    let file_name = util::file_name_of(p);
    let ext = util::ext_of(p);

    let mut stmt = conn
        .prepare("SELECT id, root_path FROM sources")
        .map_err(|e| e.to_string())?;
    let sources: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    // 取最长的匹配源（嵌套源时以更具体的为准）
    let mut best: Option<(String, String)> = None;
    for (id, root) in sources {
        if util::path_is_inside(p, std::path::Path::new(&root)) {
            let better = best
                .as_ref()
                .map(|(_, r)| root.len() > r.len())
                .unwrap_or(true);
            if better {
                best = Some((id, root));
            }
        }
    }
    let (new_source, rel_dir) = match best {
        Some((id, root)) => {
            let rel = p
                .parent()
                .and_then(|parent| parent.strip_prefix(&root).ok())
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or_default();
            (id, rel)
        }
        None => (source_id, String::new()),
    };

    if let Ok(meta) = std::fs::metadata(p) {
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        conn.execute(
            "UPDATE media_files SET absolute_path=?2, file_name=?3, ext=?4, rel_dir=?5,
             source_id=?6, size_bytes=?7, mtime_ms=?8, availability='online' WHERE id=?1",
            params![
                file_id,
                new_path,
                file_name,
                ext,
                rel_dir,
                new_source,
                meta.len() as i64,
                mtime_ms
            ],
        )
        .map_err(|e| e.to_string())?;
    } else {
        conn.execute(
            "UPDATE media_files SET absolute_path=?2, file_name=?3, ext=?4, rel_dir=?5,
             source_id=?6 WHERE id=?1",
            params![file_id, new_path, file_name, ext, rel_dir, new_source],
        )
        .map_err(|e| e.to_string())?;
    }
    recompute(conn, &asset_id)?;
    Ok(())
}
