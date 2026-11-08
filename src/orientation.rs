//! HEIC 不重新编码：在媒体库中保存方向，预览、水印和导出使用同一方向。
use image::DynamicImage;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub fn degrees(conn: &Connection, id: &str) -> i32 {
    conn.query_row(
        "SELECT degrees FROM media_rotation WHERE asset_id=?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(0)
}
pub fn load(conn: &Connection, id: &str, path: &Path) -> Result<DynamicImage, String> {
    let img = crate::media::load_oriented(path)?;
    Ok(match degrees(conn, id) {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    })
}
pub fn rotate(conn: &Connection, id: &str, delta: i32) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let angle = (degrees(&tx, id) + delta).rem_euclid(360);
    tx.execute("INSERT INTO media_rotation(asset_id,degrees) VALUES(?1,?2) ON CONFLICT(asset_id) DO UPDATE SET degrees=excluded.degrees",params![id,angle]).map_err(|e|e.to_string())?;
    tx.execute("DELETE FROM media_previews WHERE asset_id=?1", params![id])
        .map_err(|e| e.to_string())?;
    crate::assets::recompute(&tx, id)?;
    tx.commit().map_err(|e| e.to_string())
}
