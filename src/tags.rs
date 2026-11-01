//! 标签、星标与收藏：全部只写入本地索引，不触碰任何原文件。

use crate::db::R;
use crate::util;
use rusqlite::{params, Connection, OptionalExtension};

const MAX_NAME: usize = 40;

fn now() -> String {
    util::now_iso()
}

/// 标签列表（带使用次数）。
pub fn list(conn: &Connection) -> R<serde_json::Value> {
    let mut stmt = conn
        .prepare(
            "SELECT t.tag_id, t.name, t.color,
                    (SELECT count(*) FROM asset_tags at WHERE at.tag_id=t.tag_id) AS n
             FROM tags t ORDER BY n DESC, t.name COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "tag_id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "color": r.get::<_, Option<String>>(2)?,
                "count": r.get::<_, i64>(3)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(serde_json::json!(rows))
}

fn clean_name(raw: &str) -> Result<String, String> {
    let name = raw.trim().trim_start_matches('#').trim().to_string();
    if name.is_empty() {
        return Err("标签名不能为空".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("标签名最长 {MAX_NAME} 个字符"));
    }
    if name.contains(',') || name.contains('\n') || name.contains('\r') {
        return Err("标签名不能包含逗号或换行".into());
    }
    Ok(name)
}

/// 按名字取标签，不存在则创建。名字不区分大小写地复用。
pub fn ensure(conn: &Connection, raw: &str) -> R<String> {
    let name = clean_name(raw)?;
    if let Some(id) = conn
        .query_row(
            "SELECT tag_id FROM tags WHERE name=?1 COLLATE NOCASE",
            params![name],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        return Ok(id);
    }
    let id = util::uuid();
    conn.execute(
        "INSERT INTO tags(tag_id,name,color,created_at) VALUES(?1,?2,NULL,?3)",
        params![id, name, now()],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn create(conn: &Connection, name: &str, color: Option<&str>) -> R<serde_json::Value> {
    let id = ensure(conn, name)?;
    if let Some(c) = color.filter(|c| !c.trim().is_empty()) {
        conn.execute(
            "UPDATE tags SET color=?2 WHERE tag_id=?1",
            params![id, c.trim()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(serde_json::json!({"tag_id": id}))
}

pub fn update(conn: &Connection, tag_id: &str, name: Option<&str>, color: Option<&str>) -> R<()> {
    if let Some(n) = name {
        let name = clean_name(n)?;
        let clash: Option<String> = conn
            .query_row(
                "SELECT tag_id FROM tags WHERE name=?1 COLLATE NOCASE AND tag_id<>?2",
                params![name, tag_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if clash.is_some() {
            return Err("已存在同名标签".into());
        }
        conn.execute("UPDATE tags SET name=?2 WHERE tag_id=?1", params![tag_id, name])
            .map_err(|e| e.to_string())?;
    }
    if let Some(c) = color {
        let value: Option<String> = if c.trim().is_empty() { None } else { Some(c.trim().to_string()) };
        conn.execute("UPDATE tags SET color=?2 WHERE tag_id=?1", params![tag_id, value])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn delete(conn: &Connection, tag_id: &str) -> R<()> {
    conn.execute("DELETE FROM asset_tags WHERE tag_id=?1", params![tag_id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM tags WHERE tag_id=?1", params![tag_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn of_asset(conn: &Connection, asset_id: &str) -> R<Vec<serde_json::Value>> {
    let mut stmt = conn
        .prepare(
            "SELECT t.tag_id,t.name,t.color FROM asset_tags at JOIN tags t ON t.tag_id=at.tag_id
             WHERE at.asset_id=?1 ORDER BY t.name COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map(params![asset_id], |r| {
            Ok(serde_json::json!({
                "tag_id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "color": r.get::<_, Option<String>>(2)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// 用给定标签名替换某个资产的标签集合。
pub fn set_asset(conn: &Connection, asset_id: &str, names: &[String]) -> R<usize> {
    conn.execute("DELETE FROM asset_tags WHERE asset_id=?1", params![asset_id])
        .map_err(|e| e.to_string())?;
    let mut n = 0;
    for raw in names {
        if raw.trim().is_empty() {
            continue;
        }
        let tag_id = ensure(conn, raw)?;
        conn.execute(
            "INSERT OR IGNORE INTO asset_tags(asset_id,tag_id,created_at) VALUES(?1,?2,?3)",
            params![asset_id, tag_id, now()],
        )
        .map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

/// 批量打标签：mode = add（默认）| remove | set。
pub fn mutate(conn: &Connection, ids: &[String], names: &[String], mode: &str) -> R<i64> {
    let mode = match mode {
        "remove" => "remove",
        "set" => "set",
        _ => "add",
    };
    let mut affected = 0i64;
    for id in ids {
        let exists: Option<i64> = conn
            .query_row("SELECT 1 FROM media_assets WHERE id=?1", params![id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if exists.is_none() {
            continue;
        }
        match mode {
            "set" => {
                set_asset(conn, id, names)?;
                affected += 1;
            }
            "remove" => {
                for raw in names {
                    let name = match clean_name(raw) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    conn.execute(
                        "DELETE FROM asset_tags WHERE asset_id=?1 AND tag_id IN
                         (SELECT tag_id FROM tags WHERE name=?2 COLLATE NOCASE)",
                        params![id, name],
                    )
                    .map_err(|e| e.to_string())?;
                }
                affected += 1;
            }
            _ => {
                for raw in names {
                    if raw.trim().is_empty() {
                        continue;
                    }
                    let tag_id = ensure(conn, raw)?;
                    conn.execute(
                        "INSERT OR IGNORE INTO asset_tags(asset_id,tag_id,created_at) VALUES(?1,?2,?3)",
                        params![id, tag_id, now()],
                    )
                    .map_err(|e| e.to_string())?;
                }
                affected += 1;
            }
        }
    }
    // 清理没有任何资产使用的标签，避免下拉框越积越多
    if mode == "remove" || mode == "set" {
        let _ = conn.execute(
            "DELETE FROM tags WHERE tag_id NOT IN (SELECT DISTINCT tag_id FROM asset_tags)",
            [],
        );
    }
    Ok(affected)
}

/// 批量星标/收藏。`None` 表示不改。
pub fn set_flags(
    conn: &Connection,
    ids: &[String],
    star: Option<bool>,
    favorite: Option<bool>,
) -> R<i64> {
    let mut n = 0i64;
    for id in ids {
        if let Some(s) = star {
            n += conn
                .execute(
                    "UPDATE media_assets SET star=?2 WHERE id=?1",
                    params![id, s as i64],
                )
                .map_err(|e| e.to_string())? as i64;
        }
        if let Some(f) = favorite {
            conn.execute(
                "UPDATE media_assets SET favorite=?2 WHERE id=?1",
                params![id, f as i64],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(n)
}
