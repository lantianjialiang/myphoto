use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};

use std::path::{Component, Path, PathBuf};

/// 本地时间（不含时区偏移），格式与前端/数据库保持一致。
pub fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

pub fn now_iso() -> String {
    now_local().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn stamp() -> String {
    now_local().format("%Y%m%d-%H%M%S").to_string()
}

pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&hasher.finalize()), total))
}

pub fn hex(bytes: &[u8]) -> String {
    const T: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(T[(b >> 4) as usize] as char);
        s.push(T[(b & 0xf) as usize] as char);
    }
    s
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 去掉 Windows 的 `\\?\` 扩展长度前缀，得到用户可读的常规路径。
pub fn clean_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    if let Some(rest) = s.strip_prefix(r"\??\") {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

/// 用于比较/去重路径的规范化键：统一小写、反斜杠、去尾部分隔符。
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let mut out = s.trim_end_matches('\\').to_string();
    if out.len() == 2 && out.ends_with(':') {
        out.push('\\');
    }
    out.to_lowercase()
}

/// child 是否位于 parent 内（含相等）。
pub fn path_is_inside(child: &Path, parent: &Path) -> bool {
    let c = path_key(child);
    let p = path_key(parent);
    if c == p {
        return true;
    }
    let p = if p.ends_with('\\') { p } else { format!("{p}\\") };
    c.starts_with(&p)
}

/// 规范化到绝对路径（不要求存在），并拒绝包含 `..` 的穿越路径。
pub fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.replace('\\', "/");
    let mut out = PathBuf::from(base);
    for part in rel.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        if part.contains(':') {
            return None;
        }
        out.push(part);
    }
    if out.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(out)
}

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 清理单个路径片段：非法字符、保留设备名、结尾的点和空格。
pub fn sanitize_segment(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        return "_".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        s = format!("{s}_");
    }
    // 超长文件名保护（Windows 单段 255 字符上限）
    if s.chars().count() > 120 {
        let keep_ext = Path::new(&s)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let stem: String = s.chars().take(120 - keep_ext.chars().count()).collect();
        s = format!("{stem}{keep_ext}");
    }
    s
}

pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt as _;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            &mut total as *mut u64,
            &mut total_free as *mut u64,
        )
    };
    if ok != 0 {
        Some(free)
    } else {
        None
    }
}

/// 在资源管理器中定位文件（本程序唯一的“外部”交互，只读）。
pub fn reveal_in_explorer(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "文件不存在或源已离线"));
    }
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()?;
    Ok(())
}

pub fn open_path(path: &Path) -> io::Result<()> {
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()?;
    Ok(())
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn file_name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_else(|| OsStr::new(""))
        .to_string_lossy()
        .to_string()
}

pub fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn media_kind(ext: &str) -> Option<&'static str> {
    match ext {
        "jpg" | "jpeg" | "png" | "webp" | "heic" | "heif" | "gif" | "bmp" | "tif" | "tiff" => {
            Some("image")
        }
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "wmv" | "3gp" => Some("video"),
        _ => None,
    }
}

/// 判断某扩展名在当前构建中是否可解码（HEIC 无解码器，仅索引 + 占位）。
pub fn decodable_image(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff")
}

/// 确保目录存在（等价于 create_dir_all，但已存在时不报错）。
#[allow(dead_code)]
pub fn ensure_dir(p: &Path) -> io::Result<()> {
    if !p.exists() {
        fs::create_dir_all(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_handles_illegal_and_reserved() {
        assert_eq!(sanitize_segment("a:b?c"), "a_b_c");
        assert_eq!(sanitize_segment("CON"), "CON_");
        assert_eq!(sanitize_segment("dir."), "dir");
        assert_eq!(sanitize_segment("  "), "_");
        assert!(!sanitize_segment(&"x".repeat(400)).chars().count() > 121);
    }

    #[test]
    fn path_traversal_is_rejected() {
        let base = Path::new("D:\\out");
        assert!(safe_join(base, "2023/2023-05/a.jpg").is_some());
        assert!(safe_join(base, "../evil/a.jpg").is_none());
        assert!(safe_join(base, "C:/Windows/a.jpg").is_none());
    }

    #[test]
    fn inside_detection_is_component_aware() {
        assert!(path_is_inside(
            Path::new("D:\\Photos\\2023\\a.jpg"),
            Path::new("D:\\Photos")
        ));
        assert!(!path_is_inside(
            Path::new("D:\\Photos2\\a.jpg"),
            Path::new("D:\\Photos")
        ));
        assert!(path_is_inside(
            Path::new("D:\\Photos"),
            Path::new("D:\\Photos")
        ));
    }
}
