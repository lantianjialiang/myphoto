//! 精确去重：先按大小聚类缩小范围，再计算完整 SHA-256；相同哈希的资产合并。

use crate::assets;
use crate::db;
use crate::jobs::JobCtl;
use crate::util;
use rusqlite::{params, Connection};
use std::path::Path;

pub fn run_hash(db_path: &Path, job: &JobCtl, all: bool) -> Result<(), String> {
    let conn = db::open(db_path)?;
    job.phase("正在挑选待计算文件…");
    let sql = if all {
        "SELECT id, absolute_path FROM media_files
         WHERE sha256 IS NULL AND availability<>'missing' AND size_bytes>0"
    } else {
        "SELECT f.id, f.absolute_path FROM media_files f
         JOIN (SELECT size_bytes FROM media_files WHERE availability<>'missing'
               GROUP BY size_bytes HAVING count(*)>1) s ON s.size_bytes=f.size_bytes
         WHERE f.sha256 IS NULL AND f.availability<>'missing' AND f.size_bytes>0"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let targets: Vec<(String, String)> = rows.filter_map(|r| r.ok()).collect();
    drop(stmt);
    job.set_total(targets.len() as i64);
    job.phase(&format!("正在计算 SHA-256（{} 个文件）…", targets.len()));

    let mut failed = 0i64;
    db::begin_write(&conn)?;
    for (idx, (id, path)) in targets.iter().enumerate() {
        if job.is_cancelled() {
            break;
        }
        match util::sha256_file(Path::new(path)) {
            Ok((hash, _size)) => {
                let _ = conn.execute(
                    "UPDATE media_files SET sha256=?2 WHERE id=?1",
                    params![id, hash],
                );
                job.ok();
            }
            Err(e) => {
                failed += 1;
                job.fail();
                let _ = db::log_op(
                    &conn,
                    "hash-error",
                    Some(path),
                    None,
                    None,
                    None,
                    "error",
                    Some(&job.id),
                    Some(&e.to_string()),
                );
            }
        }
        if idx % 100 == 0 {
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            db::begin_write(&conn)?;
        }
    }
    let cancelled = job.is_cancelled();
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    if cancelled {
        job.phase("已取消（已计算的哈希已保存，可继续）");
        return Ok(());
    }

    job.phase("正在合并相同内容…");
    let merged = merge_duplicates(&conn)?;
    let detail = format!(
        "计算 {} 个文件，合并 {} 组重复，失败 {}",
        targets.len(),
        merged,
        failed
    );
    db::log_op(
        &conn,
        "hash",
        None,
        None,
        None,
        None,
        "ok",
        Some(&job.id),
        Some(&detail),
    );
    job.phase(&detail);
    Ok(())
}

/// 把同一 SHA-256 的多个资产合并为一个（保留文件路径信息）。
pub fn merge_duplicates(conn: &Connection) -> Result<i64, String> {
    let mut stmt = conn
        .prepare(
            "SELECT sha256 FROM media_files WHERE sha256 IS NOT NULL
             GROUP BY sha256 HAVING count(DISTINCT asset_id)>1",
        )
        .map_err(|e| e.to_string())?;
    let groups: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut merged = 0i64;
    for sha in groups {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT asset_id FROM media_files WHERE sha256=?1 ORDER BY asset_id",
            )
            .map_err(|e| e.to_string())?;
        let ids: Vec<String> = stmt
            .query_map(params![sha], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        if ids.len() < 2 {
            continue;
        }
        // 保留拥有最多文件（路径最全）的资产作为规范资产
        let mut best = ids[0].clone();
        let mut best_count = 0;
        for id in &ids {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM media_files WHERE asset_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if n > best_count {
                best_count = n;
                best = id.clone();
            }
        }
        for id in &ids {
            if id != &best {
                assets::merge_into(conn, &best, id)?;
                merged += 1;
            }
        }
    }
    Ok(merged)
}

pub fn groups_json(conn: &Connection) -> Result<serde_json::Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT sha256, count(*) AS c, sum(size_bytes) AS bytes, max(size_bytes) AS one
             FROM media_files WHERE sha256 IS NOT NULL
             GROUP BY sha256 HAVING c>1 ORDER BY bytes DESC",
        )
        .map_err(|e| e.to_string())?;
    let groups: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut out = Vec::new();
    let mut recoverable = 0i64;
    for (sha, count, bytes, one) in groups {
        let mut fs = conn
            .prepare(
                "SELECT f.id,f.asset_id,f.absolute_path,f.size_bytes,f.availability,f.taken_at,
                        f.marked,f.mtime_ms,s.root_path,
                        (SELECT count(*) FROM media_files x WHERE x.asset_id=f.asset_id) AS copies
                 FROM media_files f JOIN sources s ON s.id=f.source_id
                 WHERE f.sha256=?1 ORDER BY f.availability, f.absolute_path",
            )
            .map_err(|e| e.to_string())?;
        let files: Vec<serde_json::Value> = fs
            .query_map(params![sha], |r| {
                Ok(serde_json::json!({
                    "file_id": r.get::<_, String>(0)?,
                    "asset_id": r.get::<_, String>(1)?,
                    "path": r.get::<_, String>(2)?,
                    "size_bytes": r.get::<_, i64>(3)?,
                    "availability": r.get::<_, String>(4)?,
                    "taken_at": r.get::<_, Option<String>>(5)?,
                    "marked": r.get::<_, i64>(6)? != 0,
                    "mtime_ms": r.get::<_, i64>(7)?,
                    "source_root": r.get::<_, String>(8)?,
                    "asset_file_count": r.get::<_, i64>(9)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        drop(fs);
        recoverable += one * (count - 1).max(0);
        out.push(serde_json::json!({
            "sha256": sha,
            "count": count,
            "bytes": bytes,
            "reclaimable_bytes": one * (count - 1).max(0),
            "files": files,
        }));
    }
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM media_files WHERE sha256 IS NULL AND availability<>'missing'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(serde_json::json!({
        "groups": out,
        "group_count": out.len(),
        "reclaimable_bytes": recoverable,
        "unhashed": pending,
    }))
}
