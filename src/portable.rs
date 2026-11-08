//! 程序目录内保存数据和输出；使用一致性快照导入旧版，移动后重定位内部路径。
use crate::{db, util};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

const OUTPUTS: &[&str] = &[
    "archive",
    "watermark",
    "edited",
    "albums",
    "slideshows",
    "manifests",
    "backups",
];

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.exists() || util::path_key(src) == util::path_key(dst) {
        return Ok(());
    }
    if util::path_is_inside(dst, src) {
        return Err("迁移目标位于原目录内，请换一个独立的程序目录".into());
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for item in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let kind = item.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            continue;
        }
        let target = dst.join(item.file_name());
        if kind.is_dir() {
            copy_tree(&item.path(), &target)?;
        } else if kind.is_file() {
            if target.exists() {
                if util::sha256_file(&item.path())
                    .map_err(|e| e.to_string())?
                    .0
                    != util::sha256_file(&target).map_err(|e| e.to_string())?.0
                {
                    return Err(format!(
                        "迁移目标已存在不同文件，未覆盖：{}",
                        target.display()
                    ));
                }
            } else {
                std::fs::copy(item.path(), &target).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn relocated(value: &str, mappings: &[(PathBuf, PathBuf)]) -> String {
    if !Path::new(value).is_absolute() {
        return value.to_owned();
    }
    // 整包移动后旧路径已不存在，但现存的共同父目录仍可展开 8.3 短文件名。
    let normalized_path =
        canonical_ancestor(Path::new(value)).unwrap_or_else(|_| PathBuf::from(value));
    let normalized_value = normalized_path.to_string_lossy();
    // Match the most specific old prefix first (e.g. old data/exports before old data).
    for (old, new) in mappings {
        if util::path_is_inside(&normalized_path, old) {
            let key = util::path_key(old);
            let normalized = normalized_value.replace('/', "\\");
            let suffix = normalized
                .get(key.len()..)
                .unwrap_or("")
                .trim_start_matches('\\');
            return new.join(suffix).to_string_lossy().into_owned();
        }
    }
    value.to_owned()
}

fn relocate_json(value: &mut Value, mappings: &[(PathBuf, PathBuf)]) {
    match value {
        Value::String(s) => *s = relocated(s, mappings),
        Value::Array(a) => a.iter_mut().for_each(|v| relocate_json(v, mappings)),
        Value::Object(o) => o.values_mut().for_each(|v| relocate_json(v, mappings)),
        _ => (),
    }
}

fn rebase(conn: &Connection, mappings: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    for (table, columns) in [
        ("sources", vec!["root_path"]),
        ("media_files", vec!["absolute_path"]),
        ("media_previews", vec!["cache_path"]),
        ("edit_backups", vec!["file_path", "backup_path"]),
        ("watermark_backups", vec!["file_path", "backup_path"]),
        ("archive_plans", vec!["out_dir"]),
        ("archive_plan_items", vec!["source_path", "target_path"]),
        ("job_items", vec!["source_path", "target_path"]),
        ("operation_log", vec!["source_path", "target_path"]),
    ] {
        for column in columns {
            let rows: Vec<(i64, String)> = {
                let mut st = tx
                    .prepare(&format!(
                        "SELECT rowid,{column} FROM {table} WHERE {column} IS NOT NULL"
                    ))
                    .map_err(|e| e.to_string())?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
            };
            for (row, value) in rows {
                let new = relocated(&value, mappings);
                if new != value {
                    tx.execute(
                        &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                        params![new, row],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
    }
    for (table, column) in [
        ("sources", "options_json"),
        ("watermark_presets", "config_json"),
        ("settings", "value"),
    ] {
        let rows: Vec<(i64, String)> = {
            let mut st = tx
                .prepare(&format!("SELECT rowid,{column} FROM {table}"))
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        for (row, value) in rows {
            let new = if let Ok(mut json) = serde_json::from_str::<Value>(&value) {
                relocate_json(&mut json, mappings);
                json.to_string()
            } else {
                relocated(&value, mappings)
            };
            if new != value {
                tx.execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                    params![new, row],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.execute("DELETE FROM delete_plans", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn prepare(paths: &db::AppPaths, import_legacy: bool) -> Result<(), String> {
    let root =
        util::clean_path(&std::fs::canonicalize(paths.root_dir()).map_err(|e| e.to_string())?);
    let output = root.join("exports");
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let mut mappings = Vec::new();
    let legacy = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("PhotoVideoManager"));
    let pending = paths.data_dir.join("legacy-import.db");
    let importing = import_legacy
        && !paths.db_path.exists()
        && legacy
            .as_ref()
            .is_some_and(|p| p.join("library.db").is_file());
    let db_path = if importing {
        let old = legacy.as_ref().unwrap();
        if !pending.exists() {
            let conn = Connection::open_with_flags(
                old.join("library.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| e.to_string())?;
            conn.execute("VACUUM INTO ?1", params![pending.to_string_lossy()])
                .map_err(|e| e.to_string())?;
        }
        pending.clone()
    } else {
        paths.db_path.clone()
    };
    {
        let conn = db::open(&db_path)?;
        if importing {
            let old = legacy.as_ref().unwrap();
            let previous_output = db::get_setting(&conn, "output_root")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| old.join("exports"));
            for name in OUTPUTS {
                let source = previous_output.join(name);
                let dest = output.join(name);
                copy_tree(&source, &dest)?;
                mappings.push((source, dest));
            }
            // 早期版本的备份与字体放在数据目录本身。
            copy_tree(&old.join("backups"), &output.join("backups"))?;
            copy_tree(&old.join("fonts"), &paths.data_dir.join("fonts"))?;
            mappings.push((old.join("backups"), output.join("backups")));
            mappings.push((old.clone(), paths.data_dir.clone()));
        } else if let Some(previous) = db::get_setting(&conn, "portable_root") {
            if util::path_key(Path::new(&previous)) != util::path_key(&root) {
                mappings.push((PathBuf::from(previous), root.clone()));
            }
        }
        for (old, _) in &mut mappings {
            if let Ok(canonical) = canonical_ancestor(old) {
                *old = canonical;
            }
        }
        mappings.sort_by_key(|(old, _)| std::cmp::Reverse(util::path_key(old).len()));
        if !mappings.is_empty() {
            rebase(&conn, &mappings)?;
        }
        // 已在根目录内的输出设置可以保留；旧版外部输出设置收归程序目录。
        let configured = db::get_setting(&conn, "output_root")
            .map(PathBuf::from)
            .filter(|p| util::path_is_inside(p, &root))
            .unwrap_or(output);
        db::set_setting(&conn, "output_root", &configured.to_string_lossy())?;
        db::set_setting(&conn, "portable_root", &root.to_string_lossy())?;
    }
    if importing {
        // SQLite 快照连接已关闭，但 db::open 的 WAL 常驻连接仍在；再次快照到正式文件。
        let conn = Connection::open(&pending).map_err(|e| e.to_string())?;
        conn.execute("VACUUM INTO ?1", params![paths.db_path.to_string_lossy()])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn output_path(root: &Path, input: &str) -> Result<PathBuf, String> {
    let path = Path::new(input);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        util::safe_join(root, input).ok_or("输出目录不能包含上级目录")?
    };
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("输出目录不能包含上级目录".into());
    }
    // Resolve existing ancestors as well, so a junction cannot write outside the portable folder.
    let canonical = canonical_ancestor(&path)?;
    let root = util::clean_path(&std::fs::canonicalize(root).map_err(|e| e.to_string())?);
    if !util::path_is_inside(&canonical, &root) {
        return Err("输出目录必须位于程序目录内，才能随程序迁移".into());
    }
    if util::path_is_inside(&canonical, &root.join("data")) {
        return Err("输出目录不能放在程序的 data 数据目录内".into());
    }
    Ok(canonical)
}

fn canonical_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(ancestor.file_name().ok_or("输出路径无效")?.to_os_string());
        ancestor = ancestor.parent().ok_or("输出路径无效")?;
    }
    let mut canonical =
        util::clean_path(&std::fs::canonicalize(ancestor).map_err(|e| e.to_string())?);
    for part in tail.into_iter().rev() {
        canonical.push(part);
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("gx-portable-unit-{}", util::uuid()));
        std::fs::create_dir_all(&root).unwrap();
        util::clean_path(&std::fs::canonicalize(root).unwrap())
    }
    #[test]
    fn portable_outputs_reject_escape_and_private_data() {
        let root = fixture();
        assert!(output_path(&root, "../outside").is_err());
        assert!(output_path(&root, "data/export").is_err());
        assert!(output_path(
            &root,
            &root.parent().unwrap().join("outside").to_string_lossy()
        )
        .is_err());
        assert_eq!(
            output_path(&root, "exports/new/folder").unwrap(),
            root.join("exports/new/folder")
        );
    }
    #[test]
    fn moved_paths_rebase_nested_json_and_preserve_external_media() {
        let root = fixture();
        let old = root.join("OldApp");
        let new = root.join("NewApp");
        let mappings = vec![(old.clone(), new.clone())];
        let mut cfg = serde_json::json!({"font_path":old.join("data/fonts/font.ttf"),"exclude":[old.join("exports/backups")],"external":root.join("External/photo.jpg")});
        relocate_json(&mut cfg, &mappings);
        assert_eq!(
            util::path_key(Path::new(cfg["font_path"].as_str().unwrap())),
            util::path_key(&new.join("data/fonts/font.ttf"))
        );
        assert_eq!(
            util::path_key(Path::new(cfg["exclude"][0].as_str().unwrap())),
            util::path_key(&new.join("exports/backups"))
        );
        assert_eq!(
            cfg["external"],
            root.join("External/photo.jpg").to_string_lossy().as_ref()
        );
    }
    #[test]
    fn old_stars_merge_into_collection_without_losing_favorites() {
        let root = fixture();
        let path = root.join("library.db");
        let conn = db::open(&path).unwrap();
        conn.execute("INSERT INTO media_assets(id,media_type,created_at,star) VALUES('legacy-star','image','2020-01-01',1)",[]).unwrap();
        conn.execute_batch("PRAGMA user_version=11;").unwrap();
        let migrated = db::open(&path).unwrap();
        let flags: (i64, i64) = migrated
            .query_row(
                "SELECT star,favorite FROM media_assets WHERE id='legacy-star'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(flags, (0, 1));
    }
}
