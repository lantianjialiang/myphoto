//! 幻灯片导出：把选中的照片与视频合成为一段视频（MP4）或动图（GIF）。
//!
//! 依赖 FFmpeg（可选组件）。照片用 `-loop 1 -t <时长>` 变成静止片段，
//! 视频直接接在后面，最后用 concat 滤镜拼起来；GIF 走 palettegen/paletteuse 两遍编码。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::assets::FileRow;
use crate::jobs::JobCtl;

/// 输出格式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Mp4,
    Gif,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mp4" | "video" => Some(Format::Mp4),
            "gif" => Some(Format::Gif),
            _ => None,
        }
    }
    pub fn ext(&self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Gif => "gif",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Format::Mp4 => "MP4 视频",
            Format::Gif => "GIF 动图",
        }
    }
}

pub struct Request {
    pub ids: Vec<String>,
    pub out_dir: String,
    pub file_name: String,
    pub format: Format,
    /// 每张照片停留秒数
    pub photo_seconds: f32,
    /// 每段视频截取秒数（0 表示整段）
    pub video_seconds: f32,
    /// 画布短边像素
    pub size: u32,
    /// 背景色（补边用）
    pub background: String,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            ids: Vec::new(),
            out_dir: String::new(),
            file_name: "幻灯片".into(),
            format: Format::Mp4,
            photo_seconds: 3.0,
            video_seconds: 5.0,
            size: 720,
            background: "#000000".into(),
        }
    }
}

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `#RRGGBB` → `0xRRGGBB`
fn hex_color(s: &str) -> String {
    let h = s.trim().trim_start_matches('#');
    let rgb = if h.len() >= 6 { &h[..6] } else { "000000" };
    format!("0x{}", rgb.to_ascii_uppercase())
}

/// 一段素材的描述。
struct Segment {
    path: PathBuf,
    is_video: bool,
    seconds: f32,
}

/// 生成 concat 滤镜脚本：每段统一缩放到画布内并补边。
///
/// 照片用 `-loop 1` 会产生无限输入流，所以照片段也必须显式 `trim=duration=`，
/// 否则 concat 会一直等照片段结束，任务就卡住了。
fn concat_filter(segs: &[Segment], size: u32, bg: &str) -> String {
    let s = size.max(160);
    let mut parts: Vec<String> = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        let dur = seg.seconds.max(0.2);
        let label = format!("v{i}");
        if seg.is_video {
            parts.push(format!(
                "[{idx}:v]fps=30,scale={s}:{s}:force_original_aspect_ratio=decrease,\
                 pad={s}:{s}:(ow-iw)/2:(oh-ih)/2:color={bg},setsar=1,trim=duration={dur:.2},setpts=PTS-STARTPTS[{label}]",
                idx = i, s = s, bg = hex_color(bg), dur = dur, label = label
            ));
        } else {
            parts.push(format!(
                "[{idx}:v]fps=30,scale={s}:{s}:force_original_aspect_ratio=decrease,\
                 pad={s}:{s}:(ow-iw)/2:(oh-ih)/2:color={bg},setsar=1,format=yuv420p,\
                 trim=duration={dur:.2},setpts=PTS-STARTPTS[{label}]",
                idx = i, s = s, bg = hex_color(bg), dur = dur, label = label
            ));
        }
    }
    let labels: Vec<String> = (0..segs.len()).map(|i| format!("[v{i}]")).collect();
    parts.push(format!("{}concat=n={}:v=1:a=0[vout]", labels.join(""), segs.len()));
    parts.join(";")
}

/// 视频/图片的时长探测（秒）。读不到就返回 None。
fn probe_duration(exe: &Path, src: &Path) -> Option<f32> {
    // ffmpeg 没有 ffprobe 时，用 ffmpeg 读 stderr 的 Duration
    let out = hidden(&mut Command::new(exe))
        .arg("-hide_banner")
        .arg("-i")
        .arg(src)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    // "Duration: 00:00:12.34, start: ..."
    let idx = text.find("Duration: ")?;
    let rest = &text[idx + 10..];
    let end = rest.find(',')?;
    let ts = rest[..end].trim();
    let mut parts = ts.split(':');
    let h: f32 = parts.next()?.parse().ok()?;
    let m: f32 = parts.next()?.parse().ok()?;
    let s: f32 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// 合成 MP4。
fn build_mp4(exe: &Path, segs: &[Segment], out: &Path, size: u32, bg: &str) -> Result<(), String> {
    if segs.is_empty() {
        return Err("没有可用的素材".into());
    }
    let filter = concat_filter(segs, size, bg);
    let work = std::env::temp_dir().join(format!("pvm-slide-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| format!("无法创建临时目录：{e}"))?;
    let filter_file = work.join("filter.txt");
    std::fs::write(&filter_file, filter).map_err(|e| format!("写入滤镜失败：{e}"))?;

    let mut cmd = Command::new(exe);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error");
    for seg in segs {
        if seg.is_video {
            // 用 -t 限制读取时长：只取需要的部分，不必解码整个大文件
            cmd.arg("-t")
                .arg(format!("{:.2}", seg.seconds.max(0.2)))
                .arg("-i")
                .arg(&seg.path);
        } else {
            // 照片当静止片段：用 -loop 1 + -t 限制时长
            cmd.arg("-loop")
                .arg("1")
                .arg("-t")
                .arg(format!("{:.2}", seg.seconds.max(0.2)))
                .arg("-i")
                .arg(&seg.path);
        }
    }
    // / 前缀从文件读取选项值；旧 -filter_complex_script 已在 FFmpeg 9 移除。
    cmd.arg("-/filter_complex")
        .arg(&filter_file)
        .arg("-map")
        .arg("[vout]")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("veryfast")
        .arg("-crf")
        .arg("23")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg(out);
    let res = hidden(&mut cmd).output();
    let _ = std::fs::remove_dir_all(&work);
    let out = match res {
        Ok(o) => o,
        Err(e) => return Err(format!("调用 ffmpeg 失败：{e}")),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let brief: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(4).collect();
        return Err(format!("ffmpeg 失败：{}", brief.join(" / ")));
    }
    Ok(())
}

/// 合成 GIF：先出调色板再套到视频上（两遍编码，颜色才不脏）。
fn build_gif(exe: &Path, segs: &[Segment], out: &Path, size: u32, bg: &str) -> Result<(), String> {
    if segs.is_empty() {
        return Err("没有可用的素材".into());
    }
    let s = size.max(160).min(640);
    let filter = concat_filter(segs, s, bg);
    let work = std::env::temp_dir().join(format!("pvm-slide-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| format!("无法创建临时目录：{e}"))?;
    let palette = work.join("palette.png");

    // 输入参数：素材在前，调色板在最后（输入索引 = segs.len()）
    let add_inputs = |cmd: &mut Command, with_palette: bool| {
        cmd.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");
        for seg in segs {
            if seg.is_video {
                cmd.arg("-t")
                    .arg(format!("{:.2}", seg.seconds.max(0.2)))
                    .arg("-i")
                    .arg(&seg.path);
            } else {
                cmd.arg("-loop")
                    .arg("1")
                    .arg("-t")
                    .arg(format!("{:.2}", seg.seconds.max(0.2)))
                    .arg("-i")
                    .arg(&seg.path);
            }
        }
        if with_palette {
            cmd.arg("-i").arg(&palette);
        }
    };

    // 第一遍：生成调色板
    let mut c1 = Command::new(exe);
    add_inputs(&mut c1, false);
    c1.arg("-filter_complex")
        .arg(format!("{filter};[vout]palettegen=max_colors=128:stats_mode=diff[p]"))
        .arg("-map")
        .arg("[p]")
        .arg(&palette);
    let r1 = hidden(&mut c1).output();
    let r1 = match r1 {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(format!("调用 ffmpeg 失败：{e}"));
        }
    };
    if !r1.status.success() {
        let err = String::from_utf8_lossy(&r1.stderr);
        let brief: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(4).collect();
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("生成调色板失败：{}", brief.join(" / ")));
    }

    // 第二遍：套调色板输出 GIF。
    // paletteuse 需要两个输入：视频帧 + 调色板图，且必须在同一个滤镜图里，
    // 所以写成 [vout][N:v]paletteuse=...，N 是调色板图的输入索引。
    let pal_idx = segs.len();
    let mut c2 = Command::new(exe);
    add_inputs(&mut c2, true);
    c2.arg("-filter_complex")
        .arg(format!(
            "{filter};[vout][{pal_idx}:v]paletteuse=dither=bayer:bayer_scale=3[vfinal]"
        ))
        .arg("-map")
        .arg("[vfinal]")
        .arg("-loop")
        .arg("0")
        .arg(out);
    let r2 = hidden(&mut c2).output();
    let _ = std::fs::remove_dir_all(&work);
    let out = match r2 {
        Ok(o) => o,
        Err(e) => return Err(format!("调用 ffmpeg 失败：{e}")),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let brief: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(4).collect();
        return Err(format!("ffmpeg 失败：{}", brief.join(" / ")));
    }
    Ok(())
}

/// 主流程。
pub fn run(db_path: &Path, job: &JobCtl, req: &Request) -> Result<(), String> {
    if req.ids.is_empty() {
        return Err("没有选中任何素材".into());
    }
    if req.out_dir.trim().is_empty() {
        return Err("没有指定输出目录".into());
    }
    let Some(exe) = crate::ffmpeg::locate() else {
        return Err("没有找到 ffmpeg.exe，无法合成幻灯片（放到程序目录或用 PVM_FFMPEG 指定）".into());
    };
    let conn = crate::db::open(db_path)?;
    let out_root = PathBuf::from(req.out_dir.trim());
    std::fs::create_dir_all(&out_root).map_err(|e| format!("无法创建输出目录：{e}"))?;

    // 收集素材
    let mut segs: Vec<Segment> = Vec::new();
    let photo_cache=db_path.parent().unwrap_or(Path::new(".")).join("cache").join(format!("slideshow-{}",job.id));
    let mut rendered_photos=Vec::new();
    for id in req.ids.iter() {
        let Ok(Some(file)) = crate::assets::primary_file(&conn, id) else {
            continue;
        };
        let is_video = crate::util::media_kind(&file.ext) == Some("video");
        let mut path = PathBuf::from(&file.absolute_path);
        if !path.is_file() {
            continue;
        }
        if !is_video {
            std::fs::create_dir_all(&photo_cache).map_err(|e|e.to_string())?;
            let rendered=photo_cache.join(format!("{}.jpg",rendered_photos.len()));
            let img=crate::orientation::load(&conn,id,&path)?;
            let bytes=crate::watermark::encode(&img.to_rgba8(),"jpg",95)?;
            std::fs::write(&rendered,bytes).map_err(|e|e.to_string())?;
            rendered_photos.push(rendered.clone());path=rendered;
        }
        let seconds = if is_video {
            let dur = probe_duration(&exe, &path).unwrap_or(0.0);
            if req.video_seconds > 0.0 && dur > 0.0 {
                req.video_seconds.min(dur)
            } else {
                dur.max(1.0)
            }
        } else {
            req.photo_seconds.max(0.2)
        };
        segs.push(Segment { path, is_video, seconds });
    }
    if segs.is_empty() {
        return Err("选中的素材都没有可用的文件".into());
    }

    let name = if req.file_name.trim().is_empty() {
        "幻灯片".to_string()
    } else {
        req.file_name.trim().to_string()
    };
    let safe = crate::util::sanitize_segment(&name);
    let out_path = out_root.join(format!("{}.{}", safe, req.format.ext()));

    job.phase(&format!("正在合成幻灯片（{} 段素材）…", segs.len()));
    let res = match req.format {
        Format::Mp4 => build_mp4(&exe, &segs, &out_path, req.size, &req.background),
        Format::Gif => build_gif(&exe, &segs, &out_path, req.size, &req.background),
    };
    for path in rendered_photos { let _=std::fs::remove_file(path); }
    let _=std::fs::remove_dir(&photo_cache);
    match res {
        Ok(()) => {
            job.ok();
            let bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            let total: f32 = segs.iter().map(|s| s.seconds).sum();
            crate::db::log_op(
                &conn,
                "slideshow",
                None,
                Some(&out_path.to_string_lossy()),
                None,
                None,
                "ok",
                Some(&job.id),
                Some(&format!(
                    "{}：{} 段素材，共 {:.1} 秒，{} 字节",
                    req.format.label(),
                    segs.len(),
                    total,
                    bytes
                )),
            );
            job.phase(&format!(
                "幻灯片已生成：{}（{} 段素材，{:.1} 秒，{}）",
                out_path.display(),
                segs.len(),
                total,
                crate::util::human_size(bytes as u64)
            ));
            Ok(())
        }
        Err(e) => {
            job.fail();
            Err(e)
        }
    }
}

/// 供接口用：列出可用的输出格式。
pub fn formats() -> Value {
    json!({
        "formats": [
            { "id": "mp4", "label": Format::Mp4.label(), "ext": "mp4", "needs_ffmpeg": true },
            { "id": "gif", "label": Format::Gif.label(), "ext": "gif", "needs_ffmpeg": true }
        ]
    })
}

/// 供测试用：把 FileRow 的类型判断抽出来。
#[allow(dead_code)]
fn _unused(_f: &FileRow) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parse() {
        assert_eq!(Format::parse("mp4"), Some(Format::Mp4));
        assert_eq!(Format::parse("GIF"), Some(Format::Gif));
        assert_eq!(Format::parse("avi"), None);
        assert_eq!(Format::parse(""), None);
    }

    #[test]
    fn hex_color_normalizes() {
        assert_eq!(hex_color("#FF8A1E"), "0xFF8A1E");
        assert_eq!(hex_color("ff8a1e"), "0xFF8A1E");
        assert_eq!(hex_color(""), "0x000000");
        assert_eq!(hex_color("#abc"), "0x000000");
    }

    #[test]
    fn concat_filter_builds_one_input_per_segment() {
        let segs = vec![
            Segment { path: PathBuf::from("a.jpg"), is_video: false, seconds: 3.0 },
            Segment { path: PathBuf::from("b.mp4"), is_video: true, seconds: 5.0 },
        ];
        let f = concat_filter(&segs, 720, "#000000");
        assert!(f.contains("[0:v]"), "{f}");
        assert!(f.contains("[1:v]"), "{f}");
        assert!(f.contains("concat=n=2:v=1:a=0[vout]"), "{f}");
        // 视频按 duration 截断
        assert!(f.contains("trim=duration=5.00"), "{f}");
        assert!(f.contains("pad=720:720"), "{f}");
        assert!(f.contains("color=0x000000"), "{f}");
    }

    #[test]
    fn concat_filter_trims_photos_too() {
        // 照片用 -loop 1 会产生无限流，滤镜里必须显式 trim，否则 concat 会卡住
        let segs = vec![Segment { path: PathBuf::from("a.jpg"), is_video: false, seconds: 2.5 }];
        let f = concat_filter(&segs, 720, "#000000");
        assert!(f.contains("trim=duration=2.50"), "{f}");
        assert!(f.contains("setpts=PTS-STARTPTS[v0]"), "{f}");
        assert!(f.contains("fps=30"), "照片也要统一帧率：{f}");
    }

    #[test]
    fn concat_filter_clamps_size() {
        let segs = vec![Segment { path: PathBuf::from("a.jpg"), is_video: false, seconds: 1.0 }];
        let f = concat_filter(&segs, 10, "#101010");
        assert!(f.contains("pad=160:160"), "{f}");
        assert!(f.contains("color=0x101010"), "{f}");
    }

    #[test]
    fn duration_parse_handles_common_shapes() {
        // 直接测解析逻辑：Duration: 00:01:23.45
        let t = "Duration: 00:01:23.45, start: 0.000000";
        let idx = t.find("Duration: ").unwrap();
        let rest = &t[idx + 10..];
        let end = rest.find(',').unwrap();
        let ts = rest[..end].trim();
        let mut parts = ts.split(':');
        let h: f32 = parts.next().unwrap().parse().unwrap();
        let m: f32 = parts.next().unwrap().parse().unwrap();
        let s: f32 = parts.next().unwrap().parse().unwrap();
        assert_eq!(h * 3600.0 + m * 60.0 + s, 83.45);
    }

    #[test]
    fn empty_segments_is_error() {
        let exe = PathBuf::from("nonexistent");
        let out = PathBuf::from("out.mp4");
        assert!(build_mp4(&exe, &[], &out, 720, "#000000").is_err());
        assert!(build_gif(&exe, &[], &out, 720, "#000000").is_err());
    }
}
