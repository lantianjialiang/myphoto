//! Live Photo 配对：同一目录下「照片 + 同名 MOV/MP4」视为一组实况照片。
//!
//! 规则保守：只有一对一（恰好一张照片、一个视频）才配对，多对多一律不猜，
//! 并在界面上允许手动解除。配对结果只写入索引，不移动任何文件。

use crate::db::R;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

fn is_image(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "heic" | "heif" | "tif" | "tiff" | "webp")
}

fn is_video(ext: &str) -> bool {
    matches!(ext, "mov" | "mp4" | "m4v")
}

pub fn pair_key(source_id: &str, rel_dir: &str, file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    format!(
        "{source_id}|{}|{stem}",
        rel_dir.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    )
}

/// 重新计算全库配对，返回 (配对组数, 因多对多被跳过的候选组数)。
pub fn run(conn: &Connection) -> R<(i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT f.asset_id, f.source_id, COALESCE(f.rel_dir,''), f.file_name, f.ext
             FROM media_files f
             WHERE f.availability='online'",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut images: HashMap<String, Vec<String>> = HashMap::new();
    let mut videos: HashMap<String, Vec<String>> = HashMap::new();
    for (asset_id, source_id, rel_dir, file_name, ext) in rows {
        let ext = ext.to_lowercase();
        if !is_image(&ext) && !is_video(&ext) {
            continue;
        }
        let key = pair_key(&source_id, &rel_dir, &file_name);
        let bucket = if is_video(&ext) { &mut videos } else { &mut images };
        let list = bucket.entry(key).or_default();
        if !list.contains(&asset_id) {
            list.push(asset_id.clone());
        }
    }

    conn.execute(
        "UPDATE media_assets SET live_pair_id=NULL WHERE live_pair_id IS NOT NULL",
        [],
    )
    .map_err(|e| e.to_string())?;

    let mut paired = 0i64;
    let mut ambiguous = 0i64;
    for (key, imgs) in images.iter() {
        let Some(vids) = videos.get(key) else { continue };
        if imgs.len() == 1 && vids.len() == 1 {
            conn.execute(
                "UPDATE media_assets SET live_pair_id=?2 WHERE id=?1",
                params![imgs[0], vids[0]],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE media_assets SET live_pair_id=?2 WHERE id=?1",
                params![vids[0], imgs[0]],
            )
            .map_err(|e| e.to_string())?;
            paired += 1;
        } else if imgs.len() > 1 || vids.len() > 1 {
            ambiguous += 1;
        }
    }
    Ok((paired, ambiguous))
}

/// 某个资产的实况配对伙伴（双向查询）。
pub fn companion(conn: &Connection, asset_id: &str) -> R<Option<serde_json::Value>> {
    let pair_id: Option<String> = conn
        .query_row(
            "SELECT live_pair_id FROM media_assets WHERE id=?1",
            params![asset_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    let Some(pair_id) = pair_id else { return Ok(None) };
    let row = conn
        .query_row(
            "SELECT a.id,a.media_type,a.width,a.height,a.duration_ms,a.effective_taken_at,
                    (SELECT f.file_name FROM media_files f WHERE f.asset_id=a.id
                     ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.rowid LIMIT 1),
                    (SELECT f.size_bytes FROM media_files f WHERE f.asset_id=a.id
                     ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.rowid LIMIT 1),
                    a.media_type='video'
             FROM media_assets a WHERE a.id=?1",
            params![pair_id],
            |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "media_type": r.get::<_, String>(1)?,
                    "width": r.get::<_, Option<i64>>(2)?,
                    "height": r.get::<_, Option<i64>>(3)?,
                    "duration_ms": r.get::<_, Option<i64>>(4)?,
                    "taken_at": r.get::<_, Option<String>>(5)?,
                    "file_name": r.get::<_, Option<String>>(6)?,
                    "size_bytes": r.get::<_, Option<i64>>(7)?,
                }))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(row)
}

pub fn count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM media_assets a WHERE a.live_pair_id IS NOT NULL
         AND a.media_type='image'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// 手动解除或指定配对（只写索引）。
pub fn set_pair(conn: &Connection, asset_id: &str, partner: Option<&str>) -> R<()> {
    let old: Option<String> = conn
        .query_row(
            "SELECT live_pair_id FROM media_assets WHERE id=?1",
            params![asset_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if let Some(old) = old {
        conn.execute(
            "UPDATE media_assets SET live_pair_id=NULL WHERE id=?1 AND live_pair_id=?2",
            params![old, asset_id],
        )
        .map_err(|e| e.to_string())?;
    }
    conn.execute(
        "UPDATE media_assets SET live_pair_id=?2 WHERE id=?1",
        params![asset_id, partner],
    )
    .map_err(|e| e.to_string())?;
    if let Some(p) = partner {
        conn.execute(
            "UPDATE media_assets SET live_pair_id=?2 WHERE id=?1",
            params![p, asset_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_same_folder_and_stem() {
        let a = pair_key("src1", "2023\\Trip", "IMG_1234.HEIC");
        let b = pair_key("src1", "2023/Trip", "img_1234.MOV");
        assert_eq!(a, b);
    }

    #[test]
    fn different_folder_or_stem_do_not_pair() {
        assert_ne!(
            pair_key("src1", "A", "IMG_1.HEIC"),
            pair_key("src1", "B", "IMG_1.MOV")
        );
        assert_ne!(
            pair_key("src1", "A", "IMG_1.HEIC"),
            pair_key("src1", "A", "IMG_2.MOV")
        );
    }

    #[test]
    fn ext_kinds() {
        assert!(is_image("heic"));
        assert!(!is_image("mov"));
        assert!(is_video("mov"));
        assert!(is_video("mp4"));
        assert!(!is_video("avi"));
    }
}
