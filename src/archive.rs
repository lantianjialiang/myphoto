//! 归档：按模板生成目标路径（dry-run 计划）→ 复制到临时文件 → 校验 → 原子重命名。
//! 默认永不覆盖；一切落盘变更写入 operation_log。

use crate::assets;
use crate::db;
use crate::jobs::JobCtl;
use crate::util;
use chrono::{Datelike, Timelike};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
pub struct PlanRequest {
    #[serde(default)]
    pub asset_ids: Vec<String>,
    pub out_dir: String,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub conflict: Option<String>,
    /// copy（默认，源文件保持不动）| move（校验后移动，可撤销）
    #[serde(default)]
    pub mode: Option<String>,
}

pub const UNKNOWN_DIR: &str = "Unknown-Date";

pub fn render_rel_path(
    original_name: &str,
    ext: &str,
    media_type: &str,
    camera: Option<&str>,
    taken_at: Option<&str>,
    template: &str,
) -> String {
    let Some(dt) = taken_at.and_then(crate::dateinfer::parse_iso) else {
        return format!("{UNKNOWN_DIR}\\{}", util::sanitize_segment(original_name));
    };
    let stem = Path::new(original_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| original_name.to_string());
    let mut out = String::new();
    let mut rest = template;
    while let Some(idx) = rest.find('{') {
        out.push_str(&rest[..idx]);
        let Some(end) = rest[idx..].find('}') else {
            out.push_str(&rest[idx..]);
            rest = "";
            break;
        };
        let token = &rest[idx + 1..idx + end];
        let value = match token {
            "YYYY" => format!("{:04}", dt.year()),
            "YY" => format!("{:02}", dt.year() % 100),
            "MM" => format!("{:02}", dt.month()),
            "DD" => format!("{:02}", dt.day()),
            "HH" => format!("{:02}", dt.hour()),
            "MI" => format!("{:02}", dt.minute()),
            "YYYY-MM" => format!("{:04}-{:02}", dt.year(), dt.month()),
            "YYYY-MM-DD" => format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()),
            "YYYY_MM" => format!("{:04}_{:02}", dt.year(), dt.month()),
            "original_name" => original_name.to_string(),
            "name" => stem.clone(),
            "ext" => ext.to_string(),
            "type" => {
                if media_type == "video" { "video".into() } else { "photo".into() }
            }
            "camera" => camera.map(|c| util::sanitize_segment(c)).unwrap_or_else(|| "Unknown-Camera".into()),
            "asset_id" => String::new(),
            _ => {
                out.push('{');
                out.push_str(token);
                out.push('}');
                rest = &rest[idx + end + 1..];
                continue;
            }
        };
        out.push_str(&value);
        rest = &rest[idx + end + 1..];
    }
    out.push_str(rest);
    // 分段清理，防止目录穿越与非法字符
    let mut segments: Vec<String> = Vec::new();
    for part in out.replace('\\', "/").split('/') {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        segments.push(util::sanitize_segment(part));
    }
    if segments.is_empty() {
        segments.push(util::sanitize_segment(original_name));
    }
    segments.join("\\")
}

/// 两个路径是否在同一个卷上（用于判断移动是否会退化为「复制 + 删除」）。
fn same_volume(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| -> String {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        if let Some(rest) = s.strip_prefix("\\\\") {
            // UNC：\\server\share\...
            let mut it = rest.splitn(3, '\\');
            let server = it.next().unwrap_or("");
            let share = it.next().unwrap_or("");
            format!("\\\\{server}\\{share}")
        } else {
            s.chars().take_while(|c| *c != '\\').collect()
        }
    };
    key(a) == key(b)
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1..10_000 {
        let candidate = parent.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}-{}{ext}", util::stamp()))
}

pub fn create_plan(conn: &Connection, req: &PlanRequest) -> Result<serde_json::Value, String> {
    let template = req
        .template
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(db::default_template);
    if !template.contains("{original_name}") && !template.contains("{ext}") && !template.contains("{name}") {
        return Err("命名模板必须包含 {original_name}（或 {name}/{ext}），否则同名文件会互相冲突".into());
    }
    let conflict = match req.conflict.as_deref() {
        Some("suffix") => "suffix",
        _ => "skip",
    };
    let mode = match req.mode.as_deref() {
        Some("move") => "move",
        _ => "copy",
    };
    let out_dir_raw = req.out_dir.trim();
    if out_dir_raw.is_empty() {
        return Err("请选择输出目录".into());
    }
    let out_base = PathBuf::from(out_dir_raw);
    if !out_base.is_absolute() {
        return Err("输出目录必须是绝对路径".into());
    }
    if req.asset_ids.is_empty() {
        return Err("没有选择任何媒体".into());
    }

    let mut warnings: Vec<String> = Vec::new();
    // 输出目录位于扫描源内 → 警告，并把输出目录加入该源的排除列表，避免被重复导入
    let mut stmt = conn
        .prepare("SELECT id, root_path, options_json FROM sources")
        .map_err(|e| e.to_string())?;
    let sources: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    for (id, root, options) in &sources {
        if util::path_is_inside(&out_base, Path::new(root)) {
            warnings.push(format!(
                "输出目录位于媒体源 {root} 内，已自动加入该源的排除列表，扫描时会被跳过"
            ));
            let mut opts: crate::scan::SourceOptions =
                serde_json::from_str(options).unwrap_or_default();
            let out_key = out_base.to_string_lossy().to_string();
            if !opts.excludes.iter().any(|e| util::path_key(Path::new(e)) == util::path_key(&out_base)) {
                opts.excludes.push(out_key);
                let _ = conn.execute(
                    "UPDATE sources SET options_json=?2 WHERE id=?1",
                    params![id, serde_json::to_string(&opts).unwrap_or_else(|_| "{}".into())],
                );
            }
        }
    }

    let plan_id = util::uuid();
    let mut total = 0i64;
    let mut total_bytes = 0i64;
    let mut conflicts = 0i64;
    let mut skipped = 0i64;
    let mut move_cross_device = 0i64;

    db::begin_write(&conn)?;
    let result = (|| -> Result<(), String> {
        conn.execute(
            "INSERT INTO archive_plans(id,out_dir,template,conflict,mode,created_at,status)
             VALUES(?1,?2,?3,?4,?5,?6,'planned')",
            params![plan_id, out_base.to_string_lossy(), template, conflict, mode, util::now_iso()],
        )
        .map_err(|e| e.to_string())?;

        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
        for asset_id in &req.asset_ids {
            let Some(file) = assets::primary_file(conn, asset_id)? else {
                skipped += 1;
                continue;
            };
            let asset = assets::asset_json(conn, asset_id, false)?;
            let asset = asset.unwrap_or(serde_json::json!({}));
            if file.absolute_path.is_empty() {
                skipped += 1;
                continue;
            }
            let rel = render_rel_path(
                &file.file_name,
                &file.ext,
                asset["media_type"].as_str().unwrap_or("image"),
                asset["camera"].as_str(),
                asset["taken_at"].as_str(),
                &template,
            );
            let Some(mut target) = util::safe_join(&out_base, &rel) else {
                skipped += 1;
                continue;
            };
            let key = util::path_key(&target);
            if mode == "move" && !same_volume(Path::new(&file.absolute_path), &target) {
                move_cross_device += 1;
            }
            let disk_conflict = target.exists();
            let plan_conflict = used.contains(&key);
            if disk_conflict || plan_conflict {
                conflicts += 1;
                if conflict == "suffix" {
                    target = unique_path(&target);
                } else {
                    let note = if disk_conflict {
                        "目标已存在（跳过）"
                    } else {
                        "计划内目标重复（跳过）"
                    };
                    conn.execute(
                        "INSERT INTO archive_plan_items(plan_id,file_id,asset_id,source_path,target_path,size_bytes,status,note)
                         VALUES(?1,?2,?3,?4,?5,?6,'conflict',?7)",
                        params![
                            plan_id,
                            file.id,
                            asset_id,
                            file.absolute_path,
                            target.to_string_lossy(),
                            file.size_bytes,
                            note
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    used.insert(key);
                    total += 1;
                    total_bytes += file.size_bytes;
                    continue;
                }
            }
            used.insert(util::path_key(&target));
            conn.execute(
                "INSERT INTO archive_plan_items(plan_id,file_id,asset_id,source_path,target_path,size_bytes,status,note)
                 VALUES(?1,?2,?3,?4,?5,?6,'pending',?7)",
                params![
                    plan_id,
                    file.id,
                    asset_id,
                    file.absolute_path,
                    target.to_string_lossy(),
                    file.size_bytes,
                    if file.availability != "online" { Some("源文件当前不可用") } else { None }
                ],
            )
            .map_err(|e| e.to_string())?;
            total += 1;
            total_bytes += file.size_bytes;
        }
        conn.execute(
            "UPDATE archive_plans SET total=?2,total_bytes=?3,conflicts=?4 WHERE id=?1",
            params![plan_id, total, total_bytes, conflicts],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string())?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    let free = util::free_space(&out_base)
        .or_else(|| {
            out_base
                .ancestors()
                .skip(1)
                .find(|p| p.exists())
                .and_then(util::free_space)
        })
        .unwrap_or(0);
    if free > 0 && (total_bytes as u64) > free {
        warnings.push(format!(
            "目标磁盘可用空间 {}，预计需要 {}，可能复制失败",
            util::human_size(free),
            util::human_size(total_bytes as u64)
        ));
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} 个媒体没有已索引的文件（可能在去重合并后被并入其他条目），已从计划中排除"
        ));
    }
    if mode == "move" {
        warnings.push(
            "本次为「移动」：源文件会在校验通过后从原位置移除。执行前请确认已备份，移动可按批次撤销。"
                .into(),
        );
        if move_cross_device > 0 {
            warnings.push(format!(
                "{move_cross_device} 个文件需要跨磁盘移动，会退化为「复制 → 校验 → 删除源文件」，耗时更长"
            ));
        }
    }

    Ok(serde_json::json!({
        "plan_id": plan_id,
        "out_dir": out_base.to_string_lossy(),
        "template": template,
        "conflict": conflict,
        "mode": mode,
        "total": total,
        "total_bytes": total_bytes,
        "conflicts": conflicts,
        "free_space": free,
        "warnings": warnings,
        "items": plan_items(conn, &plan_id, 300)?,
    }))
}

pub fn plan_items(conn: &Connection, plan_id: &str, limit: i64) -> Result<serde_json::Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id,source_path,target_path,size_bytes,status,note,verified_hash FROM archive_plan_items
             WHERE plan_id=?1 ORDER BY id LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map(params![plan_id, limit], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "source_path": r.get::<_, String>(1)?,
                "target_path": r.get::<_, String>(2)?,
                "size_bytes": r.get::<_, i64>(3)?,
                "status": r.get::<_, String>(4)?,
                "note": r.get::<_, Option<String>>(5)?,
                "verified_hash": r.get::<_, Option<String>>(6)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(serde_json::json!(rows))
}

pub fn get_plan(conn: &Connection, plan_id: &str) -> Result<serde_json::Value, String> {
    let row = conn
        .query_row(
            "SELECT id,out_dir,template,conflict,total,total_bytes,conflicts,created_at,status,mode,undone_at
             FROM archive_plans WHERE id=?1",
            params![plan_id],
            |r| {
                Ok(serde_json::json!({
                    "plan_id": r.get::<_, String>(0)?,
                    "out_dir": r.get::<_, String>(1)?,
                    "template": r.get::<_, String>(2)?,
                    "conflict": r.get::<_, String>(3)?,
                    "total": r.get::<_, i64>(4)?,
                    "total_bytes": r.get::<_, i64>(5)?,
                    "conflicts": r.get::<_, i64>(6)?,
                    "created_at": r.get::<_, String>(7)?,
                    "status": r.get::<_, String>(8)?,
                    "mode": r.get::<_, String>(9)?,
                    "undone_at": r.get::<_, Option<String>>(10)?,
                }))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "计划不存在".to_string())?;
    let mut value = row;
    value["items"] = plan_items(conn, plan_id, 5000)?;
    Ok(value)
}

/// 最近计划列表，供「可撤销操作」面板使用。
pub fn plans_json(conn: &Connection, limit: i64) -> Result<serde_json::Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT p.id,p.out_dir,p.mode,p.status,p.total,p.total_bytes,p.created_at,p.undone_at,
                    (SELECT count(*) FROM archive_plan_items i WHERE i.plan_id=p.id AND i.status='verified')
             FROM archive_plans p ORDER BY p.created_at DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map(params![limit], |r| {
            Ok(serde_json::json!({
                "plan_id": r.get::<_, String>(0)?,
                "out_dir": r.get::<_, String>(1)?,
                "mode": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "total": r.get::<_, i64>(4)?,
                "total_bytes": r.get::<_, i64>(5)?,
                "created_at": r.get::<_, String>(6)?,
                "undone_at": r.get::<_, Option<String>>(7)?,
                "done_items": r.get::<_, i64>(8)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(serde_json::json!(rows))
}

fn err_code(err: &std::io::Error) -> &'static str {
    match err.kind() {
        std::io::ErrorKind::NotFound => "source-missing",
        std::io::ErrorKind::PermissionDenied => "permission-denied",
        _ => "io-error",
    }
}

/// 复制单个文件：写入临时文件 → 校验大小与哈希 → 原子重命名。
fn copy_verified(
    src: &Path,
    target: &Path,
    expect_size: i64,
    expect_hash: Option<&str>,
    conflict: &str,
    tmp_tag: &str,
) -> Result<(String, PathBuf), (String, String)> {
    let mut final_target = target.to_path_buf();
    if final_target.exists() {
        if conflict == "suffix" {
            final_target = unique_path(&final_target);
        } else {
            return Err(("conflict".into(), "目标已存在，按策略跳过".into()));
        }
    }
    let parent = final_target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| (err_code(&e).into(), format!("无法创建目录：{e}")))?;
    // 临时文件名固定到条目，残留可被下次运行精确清理
    let tmp = parent.join(format!(".pvm-{tmp_tag}.part"));
    let _ = fs::remove_file(&tmp);
    let result = (|| -> Result<String, (String, String)> {
        let mut input =
            fs::File::open(src).map_err(|e| (err_code(&e).into(), format!("无法读取源文件：{e}")))?;
        let mut output = fs::File::create(&tmp)
            .map_err(|e| (err_code(&e).into(), format!("无法创建临时文件：{e}")))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut written = 0i64;
        loop {
            let n = input
                .read(&mut buf)
                .map_err(|e| (err_code(&e).into(), format!("读取失败：{e}")))?;
            if n == 0 {
                break;
            }
            output
                .write_all(&buf[..n])
                .map_err(|e| (err_code(&e).into(), format!("写入失败（可能磁盘已满）：{e}")))?;
            hasher.update(&buf[..n]);
            written += n as i64;
        }
        output
            .sync_all()
            .map_err(|e| (err_code(&e).into(), format!("落盘失败：{e}")))?;
        drop(output);
        let hash = util::hex(&hasher.finalize());
        if written != expect_size {
            return Err((
                "size-mismatch".into(),
                format!("复制字节数不符（期望 {expect_size}，实际 {written}）"),
            ));
        }
        if let Some(h) = expect_hash {
            if !h.is_empty() && !hash.eq_ignore_ascii_case(h) {
                return Err(("hash-mismatch".into(), "源文件在计划生成后被修改".into()));
            }
        }
        // 校验副本：重新读取目标临时文件计算哈希
        let (verify, vlen) = util::sha256_file(&tmp).map_err(|e| (err_code(&e).into(), format!("副本校验失败：{e}")))?;
        if verify != hash || vlen != written as u64 {
            return Err(("verify-failed".into(), "副本内容校验不通过".into()));
        }
        fs::rename(&tmp, &final_target)
            .map_err(|e| (err_code(&e).into(), format!("提交重命名失败：{e}")))?;
        Ok(hash)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|h| (h, final_target))
}

/// 移动单个文件：先算源哈希作为凭据 → 同卷 rename（跨卷退化为复制+删除）→ 校验目标。
fn move_verified(
    src: &Path,
    target: &Path,
    expect_size: i64,
    expect_hash: Option<&str>,
    conflict: &str,
    tmp_tag: &str,
) -> Result<(String, PathBuf), (String, String)> {
    let mut final_target = target.to_path_buf();
    if final_target.exists() {
        if conflict == "suffix" {
            final_target = unique_path(&final_target);
        } else {
            return Err(("conflict".into(), "目标已存在，按策略跳过".into()));
        }
    }
    if !src.exists() {
        return Err(("source-missing".into(), "源文件不存在（可能已被移动或源已离线）".into()));
    }
    let parent = final_target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|e| (err_code(&e).into(), format!("无法创建目录：{e}")))?;

    let (src_hash, src_size) = util::sha256_file(src)
        .map_err(|e| (err_code(&e).into(), format!("无法读取源文件：{e}")))?;
    if src_size != expect_size as u64 {
        return Err((
            "size-mismatch".into(),
            format!("源文件大小与索引不一致（索引 {expect_size}，实际 {src_size}）"),
        ));
    }
    if let Some(h) = expect_hash {
        if !h.is_empty() && !h.eq_ignore_ascii_case(&src_hash) {
            return Err(("hash-mismatch".into(), "源文件在计划生成后被修改".into()));
        }
    }

    match fs::rename(src, &final_target) {
        Ok(()) => {
            let (th, tlen) = util::sha256_file(&final_target)
                .map_err(|e| (err_code(&e).into(), format!("移动后校验失败：{e}")))?;
            if th != src_hash || tlen != src_size {
                let _ = fs::rename(&final_target, src);
                return Err((
                    "verify-failed".into(),
                    "移动后校验不通过，已尝试移回原位置".into(),
                ));
            }
            Ok((src_hash, final_target))
        }
        // ERROR_NOT_SAME_DEVICE：跨卷移动 → 复制 + 校验 + 删除源
        Err(e) if e.raw_os_error() == Some(17) => {
            let (hash, tgt) =
                copy_verified(src, &final_target, expect_size, Some(&src_hash), conflict, tmp_tag)?;
            fs::remove_file(src).map_err(|e| {
                (
                    "delete-failed".into(),
                    format!("副本已校验通过，但删除源文件失败：{e}"),
                )
            })?;
            Ok((hash, tgt))
        }
        Err(e) => Err((err_code(&e).into(), format!("移动失败：{e}"))),
    }
}

pub fn run_plan(db_path: &Path, job: &JobCtl, plan_id: &str) -> Result<(), String> {
    let conn = db::open(db_path)?;
    let (out_dir, conflict, template, mode): (String, String, String, String) = conn
        .query_row(
            "SELECT out_dir, conflict, template, mode FROM archive_plans WHERE id=?1",
            params![plan_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "计划不存在".to_string())?;
    let is_move = mode == "move";
    let out_base = PathBuf::from(&out_dir);
    let _ = fs::create_dir_all(&out_base);
    if !out_base.exists() {
        let _ = conn.execute(
            "UPDATE archive_plans SET status='failed' WHERE id=?1",
            params![plan_id],
        );
        return Err(format!("输出目录不可用：{out_dir}"));
    }

    // 待执行条目：待处理 + 上次失败（可重试）；已校验的条目不会重复执行
    let mut stmt = conn
        .prepare(
            "SELECT id,file_id,source_path,target_path,size_bytes FROM archive_plan_items
             WHERE plan_id=?1 AND status IN ('pending','failed') ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let items: Vec<(i64, String, String, String, i64)> = stmt
        .query_map(params![plan_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    job.set_total(items.len() as i64);
    job.phase(&format!(
        "正在{} {} 个文件…",
        if is_move { "移动" } else { "复制" },
        items.len()
    ));
    conn.execute(
        "UPDATE archive_plans SET status='running' WHERE id=?1",
        params![plan_id],
    )
    .ok();

    let verb = if is_move { "移动" } else { "复制" };
    let mut ok = 0i64;
    let mut skipped = 0i64;
    let mut failed = 0i64;
    for (idx, (item_id, file_id, source, target, size)) in items.iter().enumerate() {
        if job.is_cancelled() {
            break;
        }
        let src = Path::new(source);
        let tgt = Path::new(target);
        // 中断恢复：目标已存在且条目曾校验通过 → 不重复执行
        let already: Option<String> = conn
            .query_row(
                "SELECT status FROM archive_plan_items WHERE id=?1",
                params![item_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if already.as_deref() == Some("verified") && tgt.exists() {
            job.skip();
            skipped += 1;
            continue;
        }
        let expect_hash: Option<String> = conn
            .query_row(
                "SELECT sha256 FROM media_files WHERE id=?1",
                params![file_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let result = if is_move {
            move_verified(src, tgt, *size, expect_hash.as_deref(), &conflict, &item_id.to_string())
        } else {
            copy_verified(src, tgt, *size, expect_hash.as_deref(), &conflict, &item_id.to_string())
        };
        match result {
            Ok((hash, final_target)) => {
                let final_str = final_target.to_string_lossy().to_string();
                let note = if is_move { Some("已移动") } else { None };
                let _ = conn.execute(
                    "UPDATE archive_plan_items SET status='verified', target_path=?2, note=?3, verified_hash=?4 WHERE id=?1",
                    params![item_id, final_str, note, hash],
                );
                if is_move {
                    if let Err(e) = assets::relocate_file(&conn, file_id, &final_str) {
                        let _ = db::log_op(
                            &conn,
                            "move",
                            Some(source),
                            Some(&final_str),
                            expect_hash.as_deref(),
                            Some(&hash),
                            "warn",
                            Some(&job.id),
                            Some(&format!("文件已移动，但索引更新失败：{e}")),
                        );
                    }
                }
                db::log_op(
                    &conn,
                    if is_move { "move" } else { "copy" },
                    Some(source),
                    Some(&final_str),
                    expect_hash.as_deref(),
                    Some(&hash),
                    "ok",
                    Some(&job.id),
                    Some(&format!(
                        "模板 {template}{}",
                        if is_move { "；可撤销" } else { "" }
                    )),
                );
                job.ok();
                ok += 1;
            }
            Err((code, msg)) => {
                if code == "conflict" {
                    let _ = conn.execute(
                        "UPDATE archive_plan_items SET status='skipped', note=?2 WHERE id=?1",
                        params![item_id, msg],
                    );
                    db::log_op(
                        &conn,
                        verb,
                        Some(source),
                        Some(target),
                        None,
                        None,
                        "skipped",
                        Some(&job.id),
                        Some(&msg),
                    );
                    job.skip();
                    skipped += 1;
                } else {
                    let _ = conn.execute(
                        "UPDATE archive_plan_items SET status='failed', note=?2 WHERE id=?1",
                        params![item_id, format!("{code}: {msg}")],
                    );
                    db::log_op(
                        &conn,
                        verb,
                        Some(source),
                        Some(target),
                        None,
                        None,
                        "error",
                        Some(&job.id),
                        Some(&format!("{code}: {msg}")),
                    );
                    job.fail();
                    failed += 1;
                }
            }
        }
        if idx % 50 == 0 {
            let snap = job.snapshot();
            let _ = conn.execute(
                "UPDATE jobs SET done=?2, succeeded=?3, failed=?4, skipped=?5, total=?6 WHERE id=?1",
                params![job.id, snap.done, snap.succeeded, snap.failed, snap.skipped, snap.total],
            );
        }
    }
    let cancelled = job.is_cancelled();
    let remaining: i64 = conn
        .query_row(
            "SELECT count(*) FROM archive_plan_items WHERE plan_id=?1 AND status IN ('pending','failed')",
            params![plan_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let status = if cancelled {
        "cancelled"
    } else if remaining > 0 {
        "partial"
    } else {
        "done"
    };
    let _ = conn.execute(
        "UPDATE archive_plans SET status=?2 WHERE id=?1",
        params![plan_id, status],
    );
    let detail = if items.is_empty() {
        format!("没有待{verb}条目（已全部校验完成或已跳过）；输出目录 {out_dir}")
    } else {
        format!("{verb}成功 {ok}，跳过 {skipped}，失败 {failed}；输出目录 {out_dir}")
    };
    job.phase(&detail);
    db::log_op(
        &conn,
        "archive",
        Some(&out_dir),
        None,
        None,
        None,
        if cancelled { "cancelled" } else { "ok" },
        Some(&job.id),
        Some(&detail),
    );
    Ok(())
}

/// 撤销一个批次：
/// - 复制批次：逐个校验副本 SHA-256 与记录一致后删除副本（绝不删除身份不明的文件）；
/// - 移动批次：校验目标内容后移回原路径，原位置若已有文件则跳过并记录，绝不覆盖。
pub fn undo_plan(db_path: &Path, job: &JobCtl, plan_id: &str) -> Result<(), String> {
    let conn = db::open(db_path)?;
    let (mode, undone_at, out_dir): (String, Option<String>, String) = conn
        .query_row(
            "SELECT mode, undone_at, out_dir FROM archive_plans WHERE id=?1",
            params![plan_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "计划不存在".to_string())?;
    if undone_at.is_some() {
        return Err("该批次已经撤销过".into());
    }
    let is_move = mode == "move";

    let mut stmt = conn
        .prepare(
            "SELECT id,file_id,source_path,target_path,verified_hash FROM archive_plan_items
             WHERE plan_id=?1 AND status='verified' ORDER BY id DESC",
        )
        .map_err(|e| e.to_string())?;
    let items: Vec<(i64, String, String, String, Option<String>)> = stmt
        .query_map(params![plan_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    if items.is_empty() {
        return Err("该批次没有可撤销的条目".into());
    }
    job.set_total(items.len() as i64);
    job.phase(&format!(
        "正在撤销 {} 个条目（{}）…",
        items.len(),
        if is_move { "移回原位置" } else { "删除已校验的副本" }
    ));

    let mut ok = 0i64;
    let mut skipped = 0i64;
    let mut failed = 0i64;
    for (item_id, file_id, source, target, vhash) in items.iter() {
        if job.is_cancelled() {
            break;
        }
        let src = Path::new(source);
        let tgt = Path::new(target);
        let fail = |code: &str, msg: String, skipped: bool| {
            let _ = conn.execute(
                "UPDATE archive_plan_items SET status=?2, note=?3 WHERE id=?1",
                params![item_id, if skipped { "undo-skipped" } else { "undo-failed" }, msg],
            );
            let _ = (code, file_id);
        };
        if !tgt.exists() {
            fail("target-missing", "目标文件已不存在（可能已被手动移走）".into(), true);
            job.skip();
            skipped += 1;
            continue;
        }
        let (hash, size) = match util::sha256_file(tgt) {
            Ok(v) => v,
            Err(e) => {
                fail("read-failed", format!("无法读取目标文件：{e}"), false);
                job.fail();
                failed += 1;
                continue;
            }
        };
        if let Some(v) = vhash.as_deref().filter(|s| !s.is_empty()) {
            if !hash.eq_ignore_ascii_case(v) {
                fail(
                    "identity-mismatch",
                    "目标内容与操作记录不一致，已拒绝删除/移动".into(),
                    false,
                );
                job.fail();
                failed += 1;
                continue;
            }
        }
        let outcome: Result<(), String> = if is_move {
            if src.exists() {
                Err("原位置已有同名文件，未覆盖".into())
            } else {
                match fs::rename(tgt, src) {
                    Ok(()) => Ok(()),
                    Err(e) if e.raw_os_error() == Some(17) => {
                        if let Some(parent) = src.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        copy_verified(tgt, src, size as i64, Some(&hash), "skip", &format!("undo{item_id}"))
                            .map_err(|(c, m)| format!("{c}: {m}"))?;
                        fs::remove_file(tgt).map_err(|e| format!("副本已移回，但删除目标失败：{e}"))?;
                        Ok(())
                    }
                    Err(e) => Err(format!("移回失败：{e}")),
                }
            }
        } else {
            fs::remove_file(tgt).map_err(|e| format!("删除副本失败：{e}"))
        };
        match outcome {
            Ok(()) => {
                if is_move {
                    if let Err(e) = assets::relocate_file(&conn, file_id, source) {
                        let _ = db::log_op(
                            &conn,
                            "undo",
                            Some(target),
                            Some(source),
                            Some(vhash.as_deref().unwrap_or("")),
                            None,
                            "warn",
                            Some(&job.id),
                            Some(&format!("文件已移回，但索引更新失败：{e}")),
                        );
                    }
                } else if let Some(parent) = tgt.parent() {
                    // 顺手清理因此变空的输出目录（remove_dir 只会在空目录上成功）
                    let _ = fs::remove_dir(parent);
                }
                let _ = conn.execute(
                    "UPDATE archive_plan_items SET status='undone', note=NULL WHERE id=?1",
                    params![item_id],
                );
                db::log_op(
                    &conn,
                    if is_move { "undo-move" } else { "undo-copy" },
                    Some(target),
                    Some(source),
                    Some(&hash),
                    None,
                    "ok",
                    Some(&job.id),
                    None,
                );
                job.ok();
                ok += 1;
            }
            Err(msg) => {
                let skipped_case = msg.contains("未覆盖");
                fail("undo-error", msg.clone(), skipped_case);
                db::log_op(
                    &conn,
                    "undo",
                    Some(target),
                    Some(source),
                    Some(&hash),
                    None,
                    if skipped_case { "skipped" } else { "error" },
                    Some(&job.id),
                    Some(&msg),
                );
                if skipped_case {
                    job.skip();
                    skipped += 1;
                } else {
                    job.fail();
                    failed += 1;
                }
            }
        }
    }

    let cancelled = job.is_cancelled();
    let remaining: i64 = conn
        .query_row(
            "SELECT count(*) FROM archive_plan_items WHERE plan_id=?1 AND status='verified'",
            params![plan_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if !cancelled && remaining == 0 {
        let _ = conn.execute(
            "UPDATE archive_plans SET undone_at=?2, status='undone' WHERE id=?1",
            params![plan_id, util::now_iso()],
        );
    }
    let detail = format!(
        "撤销完成 {}，跳过 {}，失败 {}；输出目录 {out_dir}",
        ok, skipped, failed
    );
    job.phase(&detail);
    db::log_op(
        &conn,
        "undo",
        Some(&out_dir),
        None,
        None,
        None,
        if cancelled { "cancelled" } else { "ok" },
        Some(&job.id),
        Some(&detail),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_default_template() {
        let p = render_rel_path(
            "IMG_1234.JPG",
            "jpg",
            "image",
            None,
            Some("2023-05-01T10:20:30"),
            "{YYYY}/{YYYY-MM}/{original_name}",
        );
        assert_eq!(p, "2023\\2023-05\\IMG_1234.JPG");
    }

    #[test]
    fn unknown_date_goes_to_unknown_dir() {
        let p = render_rel_path("a.jpg", "jpg", "image", None, None, "{YYYY}/{YYYY-MM}/{original_name}");
        assert_eq!(p, "Unknown-Date\\a.jpg");
    }

    #[test]
    fn template_cannot_escape_output_dir() {
        let p = render_rel_path(
            "a.jpg",
            "jpg",
            "image",
            None,
            Some("2023-05-01T00:00:00"),
            "../{YYYY}/{original_name}",
        );
        assert!(!p.contains(".."), "{p}");
    }

    #[test]
    fn illegal_characters_are_sanitized() {
        let p = render_rel_path(
            "a:b?.jpg",
            "jpg",
            "image",
            None,
            Some("2023-05-01T00:00:00"),
            "{YYYY}/{original_name}",
        );
        assert_eq!(p, "2023\\a_b_.jpg");
    }
}
