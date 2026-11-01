//! 批量图像编辑：旋转、翻转、裁剪、调色、局部打码。
//!
//! 复用水印的解码-编码管线（`media::load_oriented` → 变换 → 编码 → EXIF 回填），
//! 与水印覆盖一样：先备份原件、临时文件写入、原子替换，失败不动原图。
//!
//! 与水印覆盖不同的地方：
//!   * 变换是像素级的（旋转/镜像/裁剪/调色/打码），不产生新内容，所以「新建副本」时保持原格式；
//!   * 备份批次记在 `edit_backups` 表，可整批还原。
//!
//! 具体做了什么像素操作由 `imgops::Action` 决定，这里只管文件层面的事。

use std::path::{Path, PathBuf};

use base64::Engine;
use image::RgbaImage;
use rusqlite::params;
use serde_json::{json, Value};

use crate::assets::FileRow;
use crate::imgops::Action;
use crate::jobs::JobCtl;

pub struct Request {
    pub ids: Vec<String>,
    /// 要执行的编辑动作（旋转/翻转/裁剪/调色/打码）
    pub action: Action,
    /// true = 覆盖原文件；false = 输出到目录
    pub overwrite: bool,
    /// 覆盖时是否备份原件
    pub backup: bool,
    /// 输出目录（新建副本模式）
    pub output_dir: Option<PathBuf>,
    /// 新建副本时的文件名模板，留空则沿用原名
    pub name_template: String,
    pub keep_exif: bool,
}

fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for i in 1..1000 {
        let name = if ext.is_empty() {
            format!("{stem} ({i})")
        } else {
            format!("{stem} ({i}).{ext}")
        };
        let c = parent.join(name);
        if !c.exists() {
            return c;
        }
    }
    p.to_path_buf()
}

/// 把 EXIF 段接到 JPEG 前面，并把 Orientation 归 1（像素已经转过）。
fn splice_exif(jpeg: Vec<u8>, src: &Path, keep: bool) -> Vec<u8> {
    if !keep {
        return jpeg;
    }
    let Some(seg) = crate::watermark::exif_segment_public(src) else {
        return jpeg;
    };
    if jpeg.len() < 2 {
        return jpeg;
    }
    let mut out = Vec::with_capacity(jpeg.len() + seg.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// 编码交给水印模块统一处理，避免两处实现的格式支持不一致
/// （曾经这里自己写了一份，TIFF 会落到 JPEG 分支，写出扩展名与内容不符的文件）。
fn encode(img: &RgbaImage, ext: &str, quality: u8) -> Result<Vec<u8>, String> {
    crate::watermark::encode(img, ext, quality)
}

/// 旋转/翻转后宽高会变，索引里的 width/height 也要跟着改。
fn update_dimensions(conn: &rusqlite::Connection, asset_id: &str, w: u32, h: u32) {
    let _ = conn.execute(
        "UPDATE media_assets SET width=?1, height=?2 WHERE id=?3",
        params![w as i64, h as i64, asset_id],
    );
}

/// 覆盖后清掉缩略图缓存，否则界面还显示旋转前的图。
fn drop_previews(conn: &rusqlite::Connection, asset_id: &str) {
    let paths: Vec<String> = conn
        .prepare("SELECT cache_path FROM media_previews WHERE asset_id=?1")
        .and_then(|mut st| {
            st.query_map(params![asset_id], |r| r.get(0))
                .map(|it| it.filter_map(|x| x.ok()).collect())
        })
        .unwrap_or_default();
    for p in paths {
        let _ = std::fs::remove_file(&p);
    }
    let _ = conn.execute("DELETE FROM media_previews WHERE asset_id=?1", params![asset_id]);
}

/// 备份批次目录：数据目录\backups\edit-<jobId>\
fn backup_root(data_dir: &Path, job_id: &str, backup: bool) -> Option<PathBuf> {
    if !backup {
        return None;
    }
    Some(data_dir.join("backups").join(format!("edit-{job_id}")))
}

fn record_backup(
    conn: &rusqlite::Connection,
    job_id: &str,
    asset_id: &str,
    orig: &str,
    backup: Option<&Path>,
) -> bool {
    let Some(bp) = backup else { return false };
    let bytes = std::fs::metadata(bp).map(|m| m.len() as i64).unwrap_or(0);
    conn.execute(
        "INSERT INTO edit_backups(job_id,asset_id,file_path,backup_path,bytes,state,created_at)
         VALUES(?1,?2,?3,?4,?5,'backup',?6)",
        params![
            job_id,
            asset_id,
            orig,
            bp.to_string_lossy(),
            bytes,
            crate::util::now_iso()
        ],
    )
    .is_ok()
}

/// 备份批次列表（前端「覆盖记录」）。
pub fn backup_batches(db_path: &Path) -> Result<Vec<Value>, String> {
    let conn = crate::db::open(db_path)?;
    let mut st = conn
        .prepare(
            "SELECT job_id, count(*), sum(CASE WHEN state='restored' THEN 1 ELSE 0 END),
                    min(created_at), max(created_at)
             FROM edit_backups GROUP BY job_id ORDER BY max(id) DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows.flatten() {
        let (job, total, restored, first, last) = r;
        out.push(json!({
            "job_id": job,
            "kind": "edit",
            "total": total,
            "restored": restored.unwrap_or(0),
            "created_at": first,
            "last_at": last,
        }));
    }
    Ok(out)
}

/// 还原一个批次的编辑（从备份恢复原件）。
pub fn restore_batch(db_path: &Path, job_id: &str) -> Result<Value, String> {
    let conn = crate::db::open(db_path)?;
    let rows: Vec<(i64, String, String, String)> = {
        let mut st = conn
            .prepare("SELECT id, asset_id, file_path, backup_path FROM edit_backups WHERE job_id=?1 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map(params![job_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| e.to_string())?;
        it.flatten().collect()
    };
    if rows.is_empty() {
        return Err("找不到这个批次的备份".into());
    }
    let mut restored = 0;
    let mut failed: Vec<String> = Vec::new();
    for (id, _asset, file_path, backup_path) in rows {
        let src = PathBuf::from(&backup_path);
        let dst = PathBuf::from(&file_path);
        if !src.is_file() {
            failed.push(format!("{}：备份文件已丢失", dst.file_name().unwrap_or_default().to_string_lossy()));
            let _ = conn.execute(
                "UPDATE edit_backups SET state='missing' WHERE id=?1",
                params![id],
            );
            continue;
        }
        // 还原：先把备份内容写回原位。
        // 注意 Windows 的 rename 不能覆盖已存在的文件，所以用 copy 覆盖写，
        // 并且先留一份「当前版本」到 .pre-restore，避免还原本身不可逆。
        let safety = dst.with_file_name(format!(
            ".{}.pre-restore",
            dst.file_name().unwrap_or_default().to_string_lossy()
        ));
        let _ = std::fs::remove_file(&safety);
        let _ = std::fs::copy(&dst, &safety);
        if let Err(e) = std::fs::copy(&src, &dst) {
            failed.push(format!("{}：{e}", dst.file_name().unwrap_or_default().to_string_lossy()));
            continue;
        }
        restored += 1;
        crate::scan::refresh_modified(&conn, &dst)?;
        let _ = conn.execute(
            "UPDATE edit_backups SET state='restored', restored_at=?1 WHERE id=?2",
            params![crate::util::now_iso(), id],
        );
        // 尺寸与缩略图缓存跟着回退
        if let Ok(Some(f)) = crate::assets::primary_file(&conn, &_asset) {
            if let Some((w, h)) = crate::media::probe_dimensions(&f.absolute_path) {
                update_dimensions(&conn, &_asset, w, h);
            }
            drop_previews(&conn, &_asset);
        }
    }
    Ok(json!({
        "restored": restored,
        "failed": failed,
    }))
}

/// 删除一个批次的备份（文件也删掉）。
pub fn drop_batch(db_path: &Path, job_id: &str) -> Result<Value, String> {
    let conn = crate::db::open(db_path)?;
    let paths: Vec<String> = {
        let mut st = conn
            .prepare("SELECT backup_path FROM edit_backups WHERE job_id=?1")
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map(params![job_id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        it.flatten().collect()
    };
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    // 清理空目录
    if let Some(parent) = paths.first().and_then(|p| Path::new(p).parent()) {
        let _ = std::fs::remove_dir(parent);
    }
    let n = conn
        .execute("DELETE FROM edit_backups WHERE job_id=?1", params![job_id])
        .unwrap_or(0);
    Ok(json!({ "removed": n }))
}

/// 主流程：对一批资产执行像素编辑（旋转/翻转/裁剪/调色/打码）。
pub fn run(
    data_dir: &Path,
    db_path: &Path,
    job: &JobCtl,
    req: &Request,
) -> Result<(), String> {
    if req.ids.is_empty() {
        return Err("没有选中任何照片".into());
    }
    let conn = crate::db::open(db_path)?;
    let backup_root = backup_root(data_dir, &job.id, req.overwrite && req.backup);
    let out_root = req
        .output_dir
        .clone()
        .unwrap_or_else(|| data_dir.join("edited"));
    let quality = 92u8;

    let mut errors: Vec<String> = Vec::new();
    let mut overwritten = 0usize;

    for (idx, id) in req.ids.iter().enumerate() {
        if job.is_cancelled() { break; }
        let file = match crate::assets::primary_file(&conn, id) {
            Ok(Some(f)) => f,
            _ => {
                job.skip();
                continue;
            }
        };
        let src = PathBuf::from(&file.absolute_path);
        let src_ext = file.ext.to_lowercase();
        if crate::util::media_kind(&src_ext) == Some("video") {
            job.skip();
            continue;
        }
        // 覆盖模式必须原格式写回；新建副本时也保持原格式，避免用户拿到"扩展名变了"的怪文件
        let Some(ext) = writable_ext(&src_ext) else {
            job.fail();
            if errors.len() < 300 {
                errors.push(format!(
                    "{}：该格式不支持编辑（HEIC/HEIF 请先另存为 JPEG）",
                    file.file_name
                ));
            }
            continue;
        };

        let (target, is_overwrite) = if req.overwrite {
            (src.clone(), true)
        } else {
            // 编辑副本平铺输出到用户选的目录：这里只做像素变换，不涉及归档式的日期分层
            let stem = Path::new(&file.file_name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| file.file_name.clone());
            let name = if req.name_template.trim().is_empty() {
                format!("{stem}.{ext}")
            } else {
                let t = req.name_template.trim();
                let t = t.replace("{name}", &stem).replace("{ext}", &ext);
                if t.contains('{') {
                    format!("{stem}.{ext}")
                } else {
                    t
                }
            };
            let mut t = out_root.join(crate::util::sanitize_segment(&name));
            if let Some(parent) = t.parent() {
                if std::fs::create_dir_all(parent).is_err() {
                    job.fail();
                    continue;
                }
            }
            if t.exists() {
                t = unique_path(&t);
            }
            (t, false)
        };

        let mut backup_used: Option<PathBuf> = None;
        let result = (|| -> Result<(u32, u32), String> {
            if is_overwrite {
                if let Some(root) = &backup_root {
                    std::fs::create_dir_all(root).map_err(|e| format!("无法创建备份目录：{e}"))?;
                    let dst = unique_path(&root.join(&file.file_name));
                    std::fs::copy(&src, &dst).map_err(|e| format!("备份原件失败，已放弃修改：{e}"))?;
                    backup_used = Some(dst);
                }
            }
            let img = crate::media::load_oriented(&src)?.to_rgba8();
            let out = req.action.apply(img);
            let (w, h) = (out.width(), out.height());
            let mut bytes = encode(&out, &ext, quality)?;
            if ext == "jpg" {
                bytes = splice_exif(bytes, &src, req.keep_exif);
            }
            if job.is_cancelled() { return Err("任务已取消，当前照片未写入".into()); }
            if is_overwrite {
                let tmp = src.with_file_name(format!(".{}.pvm-edit-tmp", file.file_name));
                std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时文件失败：{e}"))?;
                std::fs::rename(&tmp, &src).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    format!("替换原文件失败：{e}")
                })?;
            } else {
                std::fs::write(&target, &bytes).map_err(|e| format!("写入失败：{e}"))?;
            }
            Ok((w, h))
        })();

        match result {
            Ok((w, h)) => {
                job.ok();
                if is_overwrite {
                    overwritten += 1;
                    // 只有裁剪/旋转会改尺寸；调色与打码后索引里的宽高依然有效
                    if req.action.changes_size() {
                        update_dimensions(&conn, &file.asset_id, w, h);
                    }
                    drop_previews(&conn, &file.asset_id);
                    crate::scan::refresh_modified(&conn, &src)?;
                    record_backup(
                        &conn,
                        &job.id,
                        &file.asset_id,
                        &file.absolute_path,
                        backup_used.as_deref(),
                    );
                    crate::db::log_op(
                        &conn,
                        "edit",
                        Some(&file.absolute_path),
                        Some(&file.absolute_path),
                        None,
                        None,
                        "ok",
                        Some(&job.id),
                        Some(&format!("{}（覆盖原图）", req.action.label())),
                    );
                } else {
                    crate::scan::register_output(&conn, id, &job.id, "edit", &target)?;
                    crate::db::log_op(
                        &conn,
                        "edit",
                        Some(&file.absolute_path),
                        Some(&target.to_string_lossy()),
                        None,
                        None,
                        "ok",
                        Some(&job.id),
                        Some(&format!("{}（新建副本）", req.action.label())),
                    );
                }
            }
            Err(e) => {
                if job.is_cancelled() { break; }
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{}：{e}", file.file_name));
                }
            }
        }
        if idx % 5 == 0 {
            job.phase(&format!("正在编辑… {}/{}", idx + 1, req.ids.len()));
        }
    }

    let p = job.snapshot();
    let mut msg = format!(
        "完成 {}（其中覆盖原图 {}），跳过 {}，失败 {}",
        p.succeeded, overwritten, p.skipped, p.failed
    );
    if let Some(root) = &backup_root {
        if root.exists() {
            msg.push_str(&format!("；原件备份：{}", root.display()));
        }
    }
    if let Some(first) = errors.first() {
        msg.push_str(&format!("；首个问题：{first}"));
    }
    job.phase(&msg);
    Ok(())
}

/// 供批量入口使用的辅助：把 FileRow 的扩展名映射到可写格式。
pub fn writable_ext(ext: &str) -> Option<String> {
    let e = ext.to_ascii_lowercase();
    match e.as_str() {
        "jpg" | "jpeg" => Some("jpg".into()),
        "png" => Some("png".into()),
        "webp" => Some("webp".into()),
        "tif" | "tiff" => Some("tif".into()),
        _ => None,
    }
}

/// 编辑预览：把动作应用到一张照片上，返回 base64 JPEG。
///
/// 前端用它做实时预览——裁剪框拖动、调色滑块变化时重新渲染，
/// 用户在执行前就能看到效果。
pub fn preview(
    db_path: &Path,
    asset_id: &str,
    action: &Action,
    max_side: u32,
) -> Result<Value, String> {
    let conn = crate::db::open(db_path)?;
    let file = crate::assets::primary_file(&conn, asset_id)?
        .ok_or_else(|| "该条目没有可用文件".to_string())?;
    if file.availability != "online" {
        return Err("原文件当前不可用（外置盘离线或已移动）".into());
    }
    let src = PathBuf::from(&file.absolute_path);
    let mut img = crate::media::load_oriented(&src)?.to_rgba8();
    // 先缩小再处理：大图裁剪/打码能快很多
    if max_side > 0 && img.width().max(img.height()) > max_side {
        let ratio = max_side as f32 / img.width().max(img.height()) as f32;
        let nw = ((img.width() as f32 * ratio).round() as u32).max(1);
        let nh = ((img.height() as f32 * ratio).round() as u32).max(1);
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    }
    let out = action.apply(img);
    let bytes = crate::watermark::encode(&out, "jpg", 88)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(json!({
        "image": format!("data:image/jpeg;base64,{b64}"),
        "width": out.width(),
        "height": out.height(),
        "file_name": file.file_name,
        "label": action.label(),
    }))
}

/// 单元测试用的小工具：确认文件行可读。
#[allow(dead_code)]
fn _unused(_f: &FileRow) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_ext_rejects_unsupported() {
        assert_eq!(writable_ext("JPG"), Some("jpg".into()));
        assert_eq!(writable_ext("heic"), None);
        assert_eq!(writable_ext("mp4"), None);
    }

    #[test]
    fn writable_ext_covers_tiff() {
        // 回归测试：TIFF 走自己的编码分支，不能落进 JPEG 分支写出坏文件
        assert_eq!(writable_ext("tif"), Some("tif".into()));
        assert_eq!(writable_ext("tiff"), Some("tif".into()));
        assert_eq!(writable_ext("PNG"), Some("png".into()));
        assert_eq!(writable_ext("webp"), Some("webp".into()));
    }

    #[test]
    fn edit_encode_tiff_is_real_tiff() {
        let mut i = RgbaImage::new(6, 3);
        for p in i.pixels_mut() {
            p.0 = [1, 2, 3, 255];
        }
        let bytes = encode(&i, "tif", 92).expect("TIFF 编码失败");
        assert!(
            bytes[..4] == [0x49, 0x49, 0x2A, 0x00] || bytes[..4] == [0x4D, 0x4D, 0x00, 0x2A],
            "TIFF 文件头不对：{:?}",
            &bytes[..4]
        );
        let back = image::load_from_memory(&bytes).expect("TIFF 无法解码");
        assert_eq!((back.width(), back.height()), (6, 3));
    }
}
