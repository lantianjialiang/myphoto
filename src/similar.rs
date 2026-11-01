//! 相似照片候选：对缩略图计算 64 位 dHash，再用汉明距离聚类。
//!
//! 只做候选提示，绝不自动删除或移动任何文件；完全相同的文件由 SHA-256 去重负责。

use crate::db;
use crate::jobs::JobCtl;
use crate::media;
use rusqlite::params;
use std::path::Path;

/// 64 位差分哈希：比较每行相邻像素的明暗关系，对整体亮度变化不敏感。
pub fn dhash_gray(img: &image::GrayImage) -> u64 {
    let small = image::imageops::resize(img, 9, 8, image::imageops::FilterType::Triangle);
    let mut bits: u64 = 0;
    let mut idx = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let a = small.get_pixel(x, y)[0];
            let b = small.get_pixel(x + 1, y)[0];
            if a > b {
                bits |= 1u64 << idx;
            }
            idx += 1;
        }
    }
    bits
}

pub fn dhash_bytes(jpeg: &[u8]) -> Result<u64, String> {
    let img = image::load_from_memory(jpeg)
        .map_err(|e| format!("解码缩略图失败：{e}"))?
        .to_luma8();
    Ok(dhash_gray(&img))
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

pub fn to_hex(h: u64) -> String {
    format!("{h:016x}")
}

pub fn from_hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim(), 16).ok()
}

/// 后台任务：为缺少 phash 的照片生成缩略图并计算 dHash。
pub fn run_hash(db_path: &Path, job: &JobCtl) -> Result<(), String> {
    let conn = db::open(db_path)?;
    job.phase("正在挑选待计算相似指纹的照片…");
    let mut stmt = conn
        .prepare(
            "SELECT a.id,
                    (SELECT x.absolute_path FROM media_files x
                     WHERE x.asset_id=a.id AND x.availability='online' ORDER BY x.rowid LIMIT 1)
             FROM media_assets a
             WHERE a.media_type='image' AND a.phash IS NULL",
        )
        .map_err(|e| e.to_string())?;
    let targets: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let total = targets.len() as i64;
    job.set_total(total);
    job.phase(&format!("正在计算相似指纹（{total} 张）…"));
    let mut failed = 0i64;
    let mut done = 0i64;
    db::begin_write(&conn)?;
    for (idx, (id, path)) in targets.iter().enumerate() {
        if job.is_cancelled() {
            break;
        }
        let Some(path) = path.as_deref().filter(|p| !p.is_empty()) else {
            job.skip();
            continue;
        };
        match media::thumbnail(Path::new(path), 64).and_then(|t| dhash_bytes(&t)) {
            Ok(h) => {
                let _ = conn.execute(
                    "UPDATE media_assets SET phash=?2 WHERE id=?1",
                    params![id, to_hex(h)],
                );
                job.ok();
                done += 1;
            }
            Err(e) => {
                failed += 1;
                job.fail();
                if failed <= 40 {
                    let _ = db::log_op(
                        &conn,
                        "phash-error",
                        Some(path),
                        None,
                        None,
                        None,
                        "error",
                        Some(&job.id),
                        Some(&e),
                    );
                }
            }
        }
        if idx % 100 == 0 {
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            db::begin_write(&conn)?;
        }
    }
    let cancelled = job.is_cancelled();
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    let detail = if cancelled {
        format!("已取消；已计算 {done} 张，失败 {failed}")
    } else {
        format!("计算 {done} 张，失败 {failed}")
    };
    db::log_op(
        &conn,
        "phash",
        None,
        None,
        None,
        None,
        if cancelled { "cancelled" } else { "ok" },
        Some(&job.id),
        Some(&detail),
    );
    job.phase(&detail);
    Ok(())
}

struct Item {
    id: String,
    hash: u64,
    sha: Option<String>,
}

fn find(parent: &mut Vec<usize>, x: usize) -> usize {
    let mut x = x;
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[rb] = ra;
    }
}

/// 按汉明距离聚类。阈值越小越严格（默认 6/64）。
pub fn groups_json(conn: &rusqlite::Connection, threshold: u32) -> Result<serde_json::Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT a.id, a.phash,
                    (SELECT x.sha256 FROM media_files x
                     WHERE x.asset_id=a.id AND x.sha256 IS NOT NULL ORDER BY x.rowid LIMIT 1)
             FROM media_assets a
             WHERE a.phash IS NOT NULL AND a.media_type='image'",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<Item> = stmt
        .query_map([], |r| {
            let h: String = r.get(1)?;
            Ok((r.get::<_, String>(0)?, h, r.get::<_, Option<String>>(2)?))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .filter_map(|(id, hex, sha)| from_hex(&hex).map(|hash| Item { id, hash, sha }))
        .collect();
    drop(stmt);

    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if hamming(rows[i].hash, rows[j].hash) <= threshold {
                union(&mut parent, i, j);
            }
        }
    }
    let mut buckets: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        buckets.entry(root).or_default().push(i);
    }

    let mut groups: Vec<serde_json::Value> = Vec::new();
    for (_, members) in buckets.into_iter() {
        if members.len() < 2 {
            continue;
        }
        // 全部 SHA-256 相同 → 属于“完全重复”，交给去重页处理，这里不重复展示
        let shas: std::collections::HashSet<&str> =
            members.iter().filter_map(|&i| rows[i].sha.as_deref()).collect();
        let all_same = shas.len() == 1 && members.iter().all(|&i| rows[i].sha.is_some());
        if all_same {
            continue;
        }
        let first = members[0];
        let mut items: Vec<serde_json::Value> = Vec::new();
        let mut max_d = 0u32;
        for &i in &members {
            let d = hamming(rows[first].hash, rows[i].hash);
            max_d = max_d.max(d);
            let info = conn
                .query_row(
                    "SELECT a.id,a.effective_taken_at,a.width,a.height,a.star,a.confidence,
                            (SELECT f.file_name FROM media_files f WHERE f.asset_id=a.id
                             ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.rowid LIMIT 1),
                            (SELECT f.size_bytes FROM media_files f WHERE f.asset_id=a.id
                             ORDER BY CASE f.availability WHEN 'online' THEN 0 ELSE 1 END, f.rowid LIMIT 1)
                     FROM media_assets a WHERE a.id=?1",
                    params![rows[i].id],
                    |r| {
                        Ok(serde_json::json!({
                            "id": r.get::<_, String>(0)?,
                            "taken_at": r.get::<_, Option<String>>(1)?,
                            "width": r.get::<_, Option<i64>>(2)?,
                            "height": r.get::<_, Option<i64>>(3)?,
                            "star": r.get::<_, i64>(4)? != 0,
                            "confidence": r.get::<_, String>(5)?,
                            "file_name": r.get::<_, Option<String>>(6)?,
                            "size_bytes": r.get::<_, Option<i64>>(7)?,
                            "distance": d,
                        }))
                    },
                )
                .map_err(|e| e.to_string())?;
            items.push(info);
        }
        items.sort_by_key(|v| v["distance"].as_u64().unwrap_or(0));
        groups.push(serde_json::json!({
            "representative": rows[first].id,
            "count": items.len(),
            "max_distance": max_d,
            "items": items,
        }));
    }
    groups.sort_by_key(|g| -(g["count"].as_i64().unwrap_or(0)));

    let unhashed: i64 = conn
        .query_row(
            "SELECT count(*) FROM media_assets WHERE media_type='image' AND phash IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(serde_json::json!({
        "groups": groups,
        "group_count": groups.len(),
        "hashed": n as i64,
        "unhashed": unhashed,
        "threshold": threshold,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(shift: i32) -> image::GrayImage {
        // 从左到右变暗：dHash 只记录相邻像素的相对明暗
        image::GrayImage::from_fn(9, 8, |x, y| {
            let v = (200 - x as i32 * 20 - y as i32 + shift).clamp(0, 255) as u8;
            image::Luma([v])
        })
    }

    #[test]
    fn identical_images_have_zero_distance() {
        let a = dhash_gray(&gradient(0));
        let b = dhash_gray(&gradient(0));
        assert_eq!(hamming(a, b), 0);
    }

    #[test]
    fn brightness_shift_keeps_hash() {
        // 整体加一个常数不改变相邻像素的相对大小
        let a = dhash_gray(&gradient(0));
        let b = dhash_gray(&gradient(4));
        assert_eq!(hamming(a, b), 0, "亮度平移不应影响差分哈希");
    }

    #[test]
    fn reversed_gradient_differs_a_lot() {
        let a = dhash_gray(&gradient(0));
        let flat = image::GrayImage::from_fn(9, 8, |_, _| image::Luma([120]));
        assert!(hamming(a, dhash_gray(&flat)) > 8);
    }

    #[test]
    fn hex_roundtrip() {
        let v = 0x0123_4567_89ab_cdefu64;
        assert_eq!(from_hex(&to_hex(v)), Some(v));
        assert_eq!(to_hex(v).len(), 16);
    }
}
