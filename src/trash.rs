use crate::{assets, db, jobs::JobCtl, util};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path, process::Command};
#[derive(Serialize, Deserialize)]
struct Item {
    file_id: String,
    path: String,
    hash: String,
}

pub fn plan(conn: &Connection, ids: &[String], all_copies: bool) -> Result<Value, String> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let kind: String = conn
            .query_row(
                "SELECT media_type FROM media_assets WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if kind != "image" {
            return Err("删除入口仅处理照片，请先取消选择视频".into());
        }
        let files = assets::files_of_asset(conn, id)?;
        for f in files
            .into_iter()
            .take(if all_copies { usize::MAX } else { 1 })
        {
            let path = Path::new(&f.absolute_path);
            if !seen.insert(util::path_key(path)) {
                continue;
            }
            let meta =
                std::fs::symlink_metadata(path).map_err(|e| format!("{}：{e}", f.file_name))?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("不能删除目录或符号链接".into());
            }
            let (hash, _) = util::sha256_file(path).map_err(|e| e.to_string())?;
            items.push(Item {
                file_id: f.id,
                path: f.absolute_path,
                hash,
            });
        }
    }
    if items.is_empty() {
        return Err("没有可删除的照片".into());
    }
    let id = util::uuid();
    let payload = serde_json::to_string(&items).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO delete_plans(id,payload) VALUES(?1,?2)",
        params![id, payload],
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"plan_id":id,"count":items.len(),"files":items.iter().map(|f|json!({"path":f.path})).collect::<Vec<_>>() }),
    )
}
pub fn take_plan(conn: &Connection, id: &str) -> Result<String, String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload FROM delete_plans WHERE id=?1 AND state='pending'",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let payload = payload.ok_or("删除计划不存在或已执行，请重新选择照片")?;
    tx.execute(
        "UPDATE delete_plans SET state='used' WHERE id=?1",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(payload)
}
fn recycle(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            include_str!("recycle_file.ps1"),
        ])
        .env("GX_RECYCLE_PATH", path)
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    if path.exists() {
        return Err("照片仍在原位置，未从媒体库移除".into());
    }
    Ok(())
}
fn forget_path(conn: &Connection, path: &str) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let affected: Vec<String> = {
        let mut st = tx
            .prepare(
                "SELECT DISTINCT asset_id FROM media_files WHERE absolute_path=?1 COLLATE NOCASE",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![path], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    tx.execute(
        "DELETE FROM media_files WHERE absolute_path=?1 COLLATE NOCASE",
        params![path],
    )
    .map_err(|e| e.to_string())?;
    for id in affected {
        let left: i64 = tx
            .query_row(
                "SELECT count(*) FROM media_files WHERE asset_id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let mut st = tx
            .prepare("SELECT cache_path FROM media_previews WHERE asset_id=?1")
            .map_err(|e| e.to_string())?;
        for p in st
            .query_map(params![id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let _ = std::fs::remove_file(p);
        }
        drop(st);
        tx.execute("DELETE FROM media_previews WHERE asset_id=?1", params![id])
            .map_err(|e| e.to_string())?;
        if left == 0 {
            tx.execute(
                "UPDATE media_assets SET live_pair_id=NULL WHERE live_pair_id=?1",
                params![id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM media_assets WHERE id=?1", params![id])
                .map_err(|e| e.to_string())?;
        } else {
            assets::recompute(&tx, &id)?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}
pub fn run(db_path: &Path, job: &JobCtl, payload: &str) -> Result<(), String> {
    let items: Vec<Item> = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    job.set_total(items.len() as i64);
    let conn = db::open(db_path)?;
    let mut errors = Vec::new();
    for item in items {
        if job.is_cancelled() {
            break;
        }
        let path = Path::new(&item.path);
        let result = (|| -> Result<(), String> {
            let indexed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM media_files WHERE id=?1 AND absolute_path=?2 COLLATE NOCASE)",params![item.file_id,item.path],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !indexed {
                return Err("确认后媒体源或索引已变化，请重新选择照片".into());
            }
            let (hash, _) = util::sha256_file(path).map_err(|e| e.to_string())?;
            if hash != item.hash {
                return Err("确认后照片已被修改，已跳过".into());
            }
            recycle(path)?;
            forget_path(&conn, &item.path)
        })();
        match result {
            Ok(()) => job.ok(),
            Err(ref e) => {
                job.fail();
                if errors.len() < 5 {
                    errors.push(format!("{}：{e}", item.path));
                }
            }
        }
        db::log_op(
            &conn,
            "recycle",
            Some(&item.path),
            None,
            Some(&item.hash),
            None,
            if result.is_ok() { "ok" } else { "error" },
            Some(&job.id),
            result.as_ref().err().map(String::as_str),
        );
    }
    let p = job.snapshot();
    job.phase(&format!(
        "移入回收站 {} 张，失败 {}。可在 Windows 回收站还原，再扫描媒体源。{}",
        p.succeeded,
        p.failed,
        errors.join("；")
    ));
    Ok(())
}
