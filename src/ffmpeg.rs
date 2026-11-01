//! 可选的 FFmpeg 集成：给视频烧录时间水印。
//!
//! 程序自身不带编码器（H.264/H.265 编码会把体积从 8 MB 拉到 80 MB+），
//! 所以这里只做"发现 + 调用"：
//!   * 查找顺序：环境变量 `PVM_FFMPEG` → 程序目录 `ffmpeg.exe` → 程序目录 `lib\ffmpeg.exe` → PATH
//!   * 文字先写到临时文件，用 `drawtext=textfile=` 读入，避免中英文冒号引号在滤镜里被转义错
//!   * 位置/字号全部用表达式（`min(w\,h)*0.03`），不需要 ffprobe 先探测分辨率

use std::path::{Path, PathBuf};
use std::process::Command;

/// 给视频用的文字样式（由 `watermark::Config` 换算而来）。
pub struct TextStyle {
    pub font: PathBuf,
    /// 字号占短边比例（0.01 = 1%）
    pub size_ratio: f32,
    pub color: String,
    pub opacity: f32,
    pub stroke: f32,
    pub stroke_color: String,
    pub shadow: f32,
    /// 底板
    pub boxed: bool,
    pub box_color: String,
    pub box_opacity: f32,
    /// 九宫格位置：第一个字符是纵向 t/m/b，第二个是横向 l/c/r；"free" 用 free_x/free_y
    pub position: String,
    /// 自由定位：水平位置占宽百分比（0-100）
    pub free_x: f32,
    /// 自由定位：垂直位置占高百分比（0-100）
    pub free_y: f32,
    /// 边距占短边比例
    pub margin: f32,
}

pub struct Status {
    pub ok: bool,
    pub path: String,
    pub version: String,
    pub hint: String,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 找到可用的 ffmpeg。
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PVM_FFMPEG") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let dir = exe_dir();
    for cand in [dir.join("ffmpeg.exe"), dir.join("lib").join("ffmpeg.exe")] {
        if cand.is_file() {
            return Some(cand);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("ffmpeg.exe");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn status() -> Status {
    match locate() {
        Some(p) => {
            let version = version(&p).unwrap_or_else(|| "版本未知".into());
            Status {
                ok: true,
                path: p.to_string_lossy().to_string(),
                version,
                hint: String::new(),
            }
        }
        None => Status {
            ok: false,
            path: String::new(),
            version: String::new(),
            hint: "把 ffmpeg.exe 放到程序目录（或 lib\\ 下）即可给视频打水印；也可以用环境变量 PVM_FFMPEG 指定路径".into(),
        },
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

pub fn version(exe: &Path) -> Option<String> {
    let mut cmd = Command::new(exe);
    cmd.arg("-version");
    let out = hidden(&mut cmd).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

/// 从系统字体目录挑一个能显示中文的字体文件。
/// `family` 是界面上选的字体名（如「微软雅黑」），找不到就用雅黑/等线/黑体兜底。
pub fn font_file(family: &str, bold: bool) -> Option<PathBuf> {
    // 自定义字体文件优先：直接就是路径
    let fam = family.trim().trim_matches('"');
    if !fam.is_empty() {
        let p = PathBuf::from(fam);
        if p.is_file() {
            return Some(p);
        }
    }
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let fonts = windir.join("Fonts");
    let mut names: Vec<&str> = Vec::new();
    let fam = family.trim();
    if fam.contains("雅黑") || fam.eq_ignore_ascii_case("Microsoft YaHei") {
        names.extend(["msyhbd.ttc", "msyh.ttc"]);
    } else if fam.contains("黑体") || fam.contains("SimHei") {
        names.extend(["simhei.ttf"]);
    } else if fam.contains("宋体") || fam.contains("SimSun") {
        names.extend(["simsun.ttc"]);
    } else if fam.contains("楷") || fam.contains("Kai") {
        names.extend(["simkai.ttf"]);
    } else if fam.contains("等线") || fam.contains("DengXian") {
        names.extend(["Deng.ttf"]);
    } else if fam.contains("思源") || fam.contains("Noto") {
        names.extend(["NotoSansSC-VF.ttf", "NotoSansCJKsc-Regular.otf"]);
    }
    if bold {
        names.extend(["msyhbd.ttc", "msyh.ttc"]);
    }
    names.extend(["msyh.ttc", "Deng.ttf", "simhei.ttf", "segoeui.ttf", "arial.ttf"]);
    for n in names {
        let p = fonts.join(n);
        if p.is_file() {
            return Some(p);
        }
    }
    // 最后在字体目录里随便找一个 ttf/ttc
    std::fs::read_dir(&fonts).ok().and_then(|rd| {
        rd.flatten()
            .map(|e| e.path())
            .find(|p| matches!(p.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref(), Some("ttf") | Some("ttc") | Some("otf")))
    })
}

fn esc_path(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

/// 把 `#RRGGBB` 或 `#RRGGBBAA` 换成 ffmpeg 的 `0xRRGGBB@a`。
fn ffmpeg_color(hex: &str, opacity: f32) -> String {
    let h = hex.trim_start_matches('#');
    let rgb = if h.len() >= 6 { &h[..6] } else { "FFFFFF" };
    format!("0x{}@{:.3}", rgb.to_ascii_uppercase(), opacity.clamp(0.0, 1.0))
}

/// 生成 filter_script 的内容。
///
/// 多行文字用「每行一个 drawtext」而不是 `line_spacing=`：ffmpeg 4.x 的
/// `line_spacing` 只认数字常量，而我们不知道真实像素高度，只好用 `th` 表达式自行堆叠。
pub fn filter_script(style: &TextStyle, text_files: &[PathBuf]) -> String {
    let is_free = style.position.eq_ignore_ascii_case("free");
    let vertical = style.position.chars().next().unwrap_or('b');
    let horizontal = style.position.chars().nth(1).unwrap_or('r');
    let m = style.margin.clamp(0.0, 0.2);
    // 自由定位：x/y 用百分比表达式，适配任意分辨率
    let x = if is_free {
        format!("w*{:.4}", (style.free_x.clamp(0.0, 100.0) / 100.0))
    } else {
        match horizontal {
            'l' => format!("w*{m:.4}"),
            'c' => "(w-tw)/2".to_string(),
            _ => format!("w-tw-w*{m:.4}"),
        }
    };
    let n = text_files.len().max(1);
    let gap = "min(w\\,h)*0.012";
    let n1 = n.saturating_sub(1);
    let y_for = |i: usize| -> String {
        if is_free {
            // 自由定位下多行往下堆叠，第一行锚在 free_y
            return format!("h*{:.4}+{i}*(th+{gap})", style.free_y.clamp(0.0, 100.0) / 100.0);
        }
        if n == 1 {
            return match vertical {
                't' => format!("h*{m:.4}"),
                'm' => "(h-th)/2".to_string(),
                _ => format!("h-th-h*{m:.4}"),
            };
        }
        // 整块文字的高度是 n*th + (n-1)*gap，按纵向锚点换算基线，最后一行才会真正贴住边距
        let base = match vertical {
            't' => format!("h*{m:.4}"),
            'm' => format!("(h-{n}*th-{n1}*({gap}))/2"),
            _ => format!("h-{n}*th-{n1}*({gap})-h*{m:.4}"),
        };
        format!("{base}+{i}*(th+{gap})")
    };

    let border_w = if style.stroke > 0.0 {
        (style.size_ratio * style.stroke * 100.0 * 0.6).clamp(0.5, 8.0)
    } else {
        0.0
    };
    text_files
        .iter()
        .enumerate()
        .map(|(i, tf)| {
            let mut parts: Vec<String> = vec![
                format!("fontfile='{}'", esc_path(&style.font)),
                format!("textfile='{}'", esc_path(tf)),
                format!("fontcolor={}", ffmpeg_color(&style.color, style.opacity)),
                format!("fontsize=min(w\\,h)*{:.4}", style.size_ratio.clamp(0.004, 0.2)),
                format!("x={x}"),
                format!("y={}", y_for(i)),
            ];
            if border_w > 0.0 {
                parts.push(format!("borderw={border_w:.2}"));
                parts.push(format!("bordercolor={}", ffmpeg_color(&style.stroke_color, 1.0)));
            }
            if style.shadow > 0.0 {
                let off = (style.size_ratio * 100.0 * style.shadow * 0.5).clamp(0.5, 6.0);
                parts.push(format!("shadowx={off:.2}"));
                parts.push(format!("shadowy={off:.2}"));
                parts.push("shadowcolor=black@0.55".to_string());
            }
            if style.boxed {
                parts.push("box=1".to_string());
                parts.push(format!("boxcolor={}", ffmpeg_color(&style.box_color, style.box_opacity)));
                parts.push(format!("boxborderw={:.0}", (style.size_ratio * 100.0 * 0.35).clamp(4.0, 40.0)));
            }
            format!("drawtext={}", parts.join(":"))
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 从视频里抽一帧存成 JPEG（用于本地生成视频封面）。
///
/// 浏览器抽帧只在浏览器能解码该编码时才有效（HEVC/AV1 常��不行），
/// 用 ffmpeg 抽帧则覆盖几乎所有容器。`at` 是秒数，取不到就退回 0 秒。
/// 返回实际写入的字节数。
pub fn extract_frame(src: &Path, dst: &Path, at: f64, max_side: u32) -> Result<u64, String> {
    let exe = locate().ok_or_else(|| "没有找到 ffmpeg.exe".to_string())?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("无法创建缓存目录：{e}"))?;
    }
    let side = max_side.clamp(64, 1920);
    // 先按请求时刻抽；很多视频第 0 秒是黑场，所以先试 at，再退回 0.5 / 0
    let mut attempts = vec![at.clamp(0.0, 600.0)];
    for fallback in [0.5_f64, 0.0_f64] {
        if !attempts.iter().any(|v| (v - fallback).abs() < 0.001) {
            attempts.push(fallback);
        }
    }
    let mut last_err = String::new();
    for t in attempts {
        let uid = uuid::Uuid::new_v4().to_string();
        let tmp = dst.with_extension(format!("tmp{}", &uid[..8]));
        let mut cmd = Command::new(&exe);
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-ss")
            .arg(format!("{t:.3}"))
            .arg("-i")
            .arg(src)
            .arg("-frames:v")
            .arg("1")
            .arg("-vf")
            .arg(format!(
                "scale='min({side},iw)':'min({side},ih)':force_original_aspect_ratio=decrease"
            ))
            .arg("-q:v")
            .arg("4")
            .arg("-f")
            .arg("mjpeg")
            .arg(&tmp);
        let out = match hidden(&mut cmd).output() {
            Ok(o) => o,
            Err(e) => {
                last_err = format!("调用 ffmpeg 失败：{e}");
                continue;
            }
        };
        let ok = out.status.success() && tmp.is_file() && std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0) > 512;
        if ok {
            let _ = std::fs::remove_file(dst);
            if let Err(e) = std::fs::rename(&tmp, dst) {
                let _ = std::fs::remove_file(&tmp);
                last_err = format!("写入封面失败：{e}");
                continue;
            }
            return Ok(std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0));
        }
        let _ = std::fs::remove_file(&tmp);
        let err = String::from_utf8_lossy(&out.stderr);
        let brief: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(3).collect();
        if !brief.is_empty() {
            last_err = brief.join(" / ");
        }
    }
    if last_err.is_empty() {
        last_err = "视频里没有可用的画面".into();
    }
    Err(last_err)
}

/// 给单个视频烧水印：`src` → `dst`。`dst` 与 `src` 相同时先写临时文件再替换。
pub fn watermark_video(src: &Path, dst: &Path, style: &TextStyle, text: &str, crf: u8) -> Result<(), String> {
    let exe = locate().ok_or_else(|| "没有找到 ffmpeg.exe".to_string())?;
    let work = std::env::temp_dir().join(format!("pvm-ffmpeg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| format!("无法创建临时目录：{e}"))?;
    // 一行一个文件，配合 drawtext 的 textfile 使用（避免任何转义问题）
    let mut text_files: Vec<PathBuf> = Vec::new();
    for (i, line) in text.replace('\r', "").split('\n').filter(|l| !l.trim().is_empty()).enumerate() {
        let p = work.join(format!("line{i}.txt"));
        std::fs::write(&p, line.as_bytes()).map_err(|e| format!("写入文字失败：{e}"))?;
        text_files.push(p);
    }
    if text_files.is_empty() {
        let p = work.join("line0.txt");
        std::fs::write(&p, text.as_bytes()).map_err(|e| format!("写入文字失败：{e}"))?;
        text_files.push(p);
    }
    let filter_file = work.join("filter.txt");
    std::fs::write(&filter_file, filter_script(style, &text_files)).map_err(|e| format!("写入滤镜失败：{e}"))?;

    let Some(ext) = dst.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()) else {
        let _ = std::fs::remove_dir_all(&work);
        return Err("输出文件没有扩展名".into());
    };
    let same = dst == src;
    let out_path = if same {
        src.with_file_name(format!(".pvm-wm-{}.{ext}", &uuid::Uuid::new_v4().to_string()[..8]))
    } else {
        dst.to_path_buf()
    };
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("无法创建输出目录：{e}"))?;
    }

    let mut cmd = Command::new(&exe);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(src)
        .arg("-filter_script:v")
        .arg(&filter_file)
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("veryfast")
        .arg("-crf")
        .arg(crf.clamp(14, 30).to_string())
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("copy")
        .arg("-map_metadata")
        .arg("0");
    if matches!(ext.as_str(), "mp4" | "m4v" | "mov") {
        cmd.arg("-movflags").arg("+faststart");
    }
    cmd.arg(&out_path);
    let out = hidden(&mut cmd)
        .output()
        .map_err(|e| format!("调用 ffmpeg 失败：{e}"))?;
    let _ = std::fs::remove_dir_all(&work);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let brief: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).take(4).collect();
        if same {
            let _ = std::fs::remove_file(&out_path);
        }
        return Err(format!("ffmpeg 失败：{}", brief.join(" / ")));
    }
    if same {
        std::fs::rename(&out_path, src).map_err(|e| {
            let _ = std::fs::remove_file(&out_path);
            format!("替换原视频失败：{e}")
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> TextStyle {
        TextStyle {
            font: PathBuf::from("C:\\Windows\\Fonts\\msyh.ttc"),
            size_ratio: 0.03,
            color: "#FF8A1E".into(),
            opacity: 0.95,
            stroke: 0.05,
            stroke_color: "#20120A".into(),
            shadow: 0.1,
            boxed: false,
            box_color: "#000000".into(),
            box_opacity: 0.42,
            position: "br".into(),
            free_x: 0.0,
            free_y: 0.0,
            margin: 0.03,
        }
    }

    #[test]
    fn filter_uses_textfile_and_expressions() {
        let f = filter_script(&style(), &[PathBuf::from("C:\\Temp\\t.txt")]);
        assert!(f.starts_with("drawtext="));
        assert!(f.contains("textfile='C\\:/Temp/t.txt'"), "{f}");
        assert!(f.contains("fontfile='C\\:/Windows/Fonts/msyh.ttc'"), "{f}");
        assert!(f.contains("fontsize=min(w\\,h)*0.0300"), "{f}");
        assert!(f.contains("x=w-tw-w*0.0300"), "{f}");
        assert!(f.contains("y=h-th-h*0.0300"), "{f}");
        assert!(!f.contains('\n'), "filter 必须是单行");
    }

    #[test]
    fn filter_stacks_two_lines() {
        let f = filter_script(
            &style(),
            &[PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        );
        assert_eq!(f.matches("drawtext=").count(), 2, "{f}");
        // 两行时整块上移一个 gap，最后一行才贴住边距
        assert!(f.contains("y=h-2*th-1*(min(w\\,h)*0.012)-h*0.0300+0*(th+min(w\\,h)*0.012)"), "{f}");
        assert!(f.contains("y=h-2*th-1*(min(w\\,h)*0.012)-h*0.0300+1*(th+min(w\\,h)*0.012)"), "{f}");
    }

    #[test]
    fn filter_box_and_center() {
        let mut s = style();
        s.boxed = true;
        s.position = "mc".into();
        let f = filter_script(&s, &[PathBuf::from("t.txt")]);
        assert!(f.contains("x=(w-tw)/2"), "{f}");
        assert!(f.contains("y=(h-th)/2"), "{f}");
        assert!(f.contains("box=1"), "{f}");
        assert!(f.contains("boxcolor=0x000000@0.420"), "{f}");
    }

    #[test]
    fn color_conversion() {
        assert_eq!(ffmpeg_color("#FF8A1E", 1.0), "0xFF8A1E@1.000");
        assert_eq!(ffmpeg_color("00ff00", 0.5), "0x00FF00@0.500");
    }

    #[test]
    fn free_position_uses_percent_expressions() {
        let mut s = style();
        s.position = "free".into();
        s.free_x = 12.5;
        s.free_y = 33.0;
        let f = filter_script(&s, &[PathBuf::from("t.txt")]);
        assert!(f.contains("x=w*0.1250"), "{f}");
        assert!(f.contains("y=h*0.3300"), "{f}");
        // 自由定位下不应出现九宫格的居中表达式
        assert!(!f.contains("(w-tw)/2"), "{f}");
        assert!(!f.contains("h-th-h*"), "{f}");
    }

    #[test]
    fn free_position_stacks_lines() {
        let mut s = style();
        s.position = "free".into();
        s.free_x = 10.0;
        s.free_y = 20.0;
        let f = filter_script(&s, &[PathBuf::from("a.txt"), PathBuf::from("b.txt")]);
        assert_eq!(f.matches("drawtext=").count(), 2, "{f}");
        assert!(f.contains("y=h*0.2000+0*(th+min(w\\,h)*0.012)"), "{f}");
        assert!(f.contains("y=h*0.2000+1*(th+min(w\\,h)*0.012)"), "{f}");
    }

    #[test]
    fn extract_frame_writes_jpeg() {
        let Some(exe) = locate() else {
            eprintln!("跳过：环境里没有 ffmpeg");
            return;
        };
        // 造一个 2 秒的测试视频
        let dir = std::env::temp_dir().join(format!("pvm-fm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mp4 = dir.join("s.mp4");
        let out = hidden(&mut Command::new(&exe))
            .args([
                "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "testsrc=duration=2:size=320x240:rate=10", "-pix_fmt", "yuv420p",
            ])
            .arg(&mp4)
            .output()
            .unwrap();
        assert!(out.status.success(), "生成测试视频失败");
        let jpg = dir.join("f.jpg");
        let n = extract_frame(&mp4, &jpg, 0.5, 640).expect("抽帧失败");
        assert!(n > 512, "抽出来的图太小：{n}");
        let head = std::fs::read(&jpg).unwrap();
        assert_eq!(&head[..2], &[0xFF, 0xD8], "不是 JPEG");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_frame_reports_failure_on_garbage() {
        if locate().is_none() {
            eprintln!("跳过：环境里没有 ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("pvm-fm-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.mp4");
        std::fs::write(&bad, b"not a video at all").unwrap();
        let jpg = dir.join("f.jpg");
        let r = extract_frame(&bad, &jpg, 0.0, 640);
        assert!(r.is_err(), "对损坏文件应当报错");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
