//! 时间水印：把拍摄时间（或自定义文字）烧进图片**副本**，原图永不改动。
//!
//! 关键设计：
//! - 字号按图片短边比例计算（`size_pct`），再夹在 `min_px`/`max_px` 之间，
//!   并受 `max_width_pct` 保护，避免高分辨率图上看不见、低分辨率图上糊满屏。
//! - 先按 EXIF 方向校正再排版定位，否则竖拍照片的水印会跑到错误角落。
//! - 日期来源沿用索引里的 `effective_taken_at`（已含人工覆盖与时间偏移），
//!   无可靠时间时按配置跳过，不猜。

use crate::assets;
use crate::jobs::JobCtl;
use crate::media;
use crate::util;
use ab_glyph::{Font, FontVec, Glyph, GlyphId, PxScale, ScaleFont};
use base64::Engine;
use image::{imageops::FilterType, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 九宫格锚点：tl tc tr ml mc mr bl bc br；"free" 表示用 pos_x/pos_y 自由定位
    pub position: String,
    /// 自由定位：水平位置，水印左边缘占图宽百分比（0-100）
    pub pos_x: f64,
    /// 自由定位：垂直位置，水印上边缘占图高百分比（0-100）
    pub pos_y: f64,
    /// 边距，占短边百分比
    pub margin_pct: f64,
    /// 字号，占短边百分比
    pub size_pct: f64,
    pub min_px: f64,
    pub max_px: f64,
    /// 水印宽度上限，占图片宽度百分比
    pub max_width_pct: f64,
    pub font: String,
    /// 自定义字体文件路径（.ttf/.ttc/.otf）；非空时优先于 font 指定的字体名
    pub font_path: String,
    pub bold: bool,
    pub color: String,
    pub opacity: f64,
    /// 描边宽度，相对字号
    pub stroke: f64,
    pub stroke_color: String,
    /// 阴影偏移，相对字号；0 关闭
    pub shadow: f64,
    pub shadow_color: String,
    pub plate: bool,
    pub plate_color: String,
    pub plate_opacity: f64,
    pub plate_radius: f64,
    pub plate_pad: f64,
    /// 底板铺满整幅宽度（相框底栏风格）
    pub plate_full: bool,
    /// 旋转角度（度）
    pub rotate: f64,
    /// 画面方向校正（0/90/180/270）：竖拍但 EXIF 缺失或方向错误的照片用这个转正
    pub rotate_image: i32,
    /// chrono strftime 格式
    pub date_format: String,
    /// 文字模板，支持 {date} {time} {filename} {camera} {source}
    pub text: String,
    /// 追加一行自定义文字
    pub custom: String,
    /// 是否在末尾标注时间来源
    pub source_hint: bool,
    /// 无可靠拍摄时间时跳过（默认跳过，不猜）
    pub require_date: bool,
    /// same | jpg | png | webp
    pub output_format: String,
    /// 视频也烧水印（需要 ffmpeg.exe；没有就跳过视频）
    pub video: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            position: "bc".into(),
            pos_x: 50.0,
            pos_y: 50.0,
            margin_pct: 3.0,
            size_pct: 2.8,
            min_px: 12.0,
            max_px: 200.0,
            max_width_pct: 40.0,
            font: "微软雅黑".into(),
            font_path: String::new(),
            bold: true,
            color: "#FF8A1E".into(),
            opacity: 1.0,
            stroke: 0.05,
            stroke_color: "#000000".into(),
            shadow: 0.10,
            shadow_color: "#000000".into(),
            plate: false,
            plate_color: "#000000".into(),
            plate_opacity: 0.42,
            plate_radius: 0.22,
            plate_pad: 0.34,
            plate_full: false,
            rotate: 0.0,
            rotate_image: 0,
            date_format: "%Y-%m-%d".into(),
            text: "{date}".into(),
            custom: String::new(),
            source_hint: false,
            require_date: true,
            output_format: "same".into(),
            video: true,
        }
    }
}

impl Config {
    fn clean(mut self) -> Config {
        self.size_pct = self.size_pct.clamp(0.5, 30.0);
        self.min_px = self.min_px.clamp(6.0, 400.0);
        self.max_px = self.max_px.clamp(self.min_px, 800.0);
        self.margin_pct = self.margin_pct.clamp(0.0, 30.0);
        self.max_width_pct = self.max_width_pct.clamp(5.0, 100.0);
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.stroke = self.stroke.clamp(0.0, 0.4);
        self.shadow = self.shadow.clamp(0.0, 0.5);
        self.plate_opacity = self.plate_opacity.clamp(0.0, 1.0);
        self.plate_radius = self.plate_radius.clamp(0.0, 2.0);
        self.plate_pad = self.plate_pad.clamp(0.0, 2.0);
        self.rotate = ((self.rotate % 360.0) + 360.0) % 360.0;
        self.rotate_image = ((self.rotate_image % 360) + 360) % 360;
        // 只接受 90 的整数倍
        self.rotate_image = (self.rotate_image / 90).rem_euclid(4) * 90;
        self.pos_x = self.pos_x.clamp(0.0, 100.0);
        self.pos_y = self.pos_y.clamp(0.0, 100.0);
        if self.size_pct <= 0.0 {
            self.size_pct = 2.8;
        }
        self
    }
}

// ---------------- 字体 ----------------

/// 系统里常见字体。ttc 用第 0 个字形集（Windows 的中文字体都是这样排的）。
const FONT_CANDIDATES: &[(&str, &str, &str)] = &[
    ("微软雅黑", "msyh.ttc", "msyhbd.ttc"),
    ("黑体", "simhei.ttf", "simhei.ttf"),
    ("宋体", "simsun.ttc", "simsunb.ttf"),
    ("等线", "Deng.ttf", "Dengb.ttf"),
    ("Consolas", "consola.ttf", "consolab.ttf"),
    ("Segoe UI", "segoeui.ttf", "segoeuib.ttf"),
    ("Arial", "arial.ttf", "arialbd.ttf"),
    ("Georgia", "georgia.ttf", "georgiab.ttf"),
    ("Times New Roman", "times.ttf", "timesbd.ttf"),
    ("Impact", "impact.ttf", "impact.ttf"),
];

fn fonts_dir() -> PathBuf {
    let win = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    win.join("Fonts")
}

pub fn font_choices() -> Value {
    let dir = fonts_dir();
    let list: Vec<Value> = FONT_CANDIDATES
        .iter()
        .filter(|(_, reg, _)| dir.join(reg).exists() || dir.join(reg.to_lowercase()).exists())
        .map(|(name, reg, bold)| {
            json!({"name": name, "file": reg, "bold": bold, "path": dir.join(reg).to_string_lossy()})
        })
        .collect();
    json!({"fonts": list, "dir": dir.to_string_lossy()})
}

/// 扫描一个目录里的字体文件（用于"自定义字体"目录）。
pub fn scan_font_dir(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if !p.is_file() || !is_font_file(&p) {
            continue;
        }
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        out.push(json!({
            "name": name,
            "file": p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            "path": p.to_string_lossy(),
        }));
    }
    out
}

/// 校验一个自定义字体文件：能否读到、能否被 ab_glyph 解析。
pub fn validate_custom_font(spec: &str) -> Value {
    let s = spec.trim().trim_matches('"');
    if s.is_empty() {
        return json!({"ok": false, "error": "没有指定字体文件"});
    }
    let Some(path) = custom_font_file(s) else {
        return json!({
            "ok": false,
            "error": "找不到这个字体文件，或不是 .ttf/.ttc/.otf",
            "path": s,
        });
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return json!({"ok": false, "error": format!("读取失败：{e}"), "path": path.to_string_lossy()})
        }
    };
    match FontVec::try_from_vec_and_index(bytes, 0) {
        Ok(_) => json!({
            "ok": true,
            "path": path.to_string_lossy(),
            "name": font_family_name(&path),
            "size": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        }),
        Err(_) => json!({
            "ok": false,
            "error": "这个文件不是有效的字体（可能已损坏或格式不受支持）",
            "path": path.to_string_lossy(),
        }),
    }
}

fn font_cache() -> &'static Mutex<HashMap<(String, bool), Arc<FontVec>>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, bool), Arc<FontVec>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 字体文件是否是我们支持的扩展名。
pub fn is_font_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("ttf") | Some("ttc") | Some("otf")
    )
}

/// 自定义字体文件：绝对路径或相对当前工作目录的路径都接受。
/// 返回 None 表示路径无效或不是字体文件。
pub fn custom_font_file(spec: &str) -> Option<PathBuf> {
    let s = spec.trim().trim_matches('"');
    if s.is_empty() {
        return None;
    }
    let p = PathBuf::from(s);
    if !p.is_absolute() {
        // 允许只写文件名：在系统字体目录里找
        let in_win = fonts_dir().join(s);
        if in_win.is_file() {
            return Some(in_win);
        }
    }
    if p.is_file() && is_font_file(&p) {
        return Some(p);
    }
    None
}

/// 读取字体文件里的族名（用于界面显示），失败时返回文件名。
pub fn font_family_name(path: &Path) -> String {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(f) = FontVec::try_from_vec_and_index(bytes.clone(), 0) {
            // ab_glyph 不暴露族名，退而用文件名更实用
            let _ = f;
        }
        if let Some(f) = fontdb_probe(&bytes) {
            return f;
        }
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "自定义字体".into())
}

/// 从字体二进制里粗略提取族名（读 name 表）。
fn fontdb_probe(bytes: &[u8]) -> Option<String> {
    // 只做最小可用：找 name 表里的 family 字符串，失败就返回 None
    let text = String::from_utf8_lossy(bytes);
    // 常见族名以 1 开始的长度前缀 UTF-16BE 存放，直接在明文里找更省事
    for cand in ["Microsoft YaHei", "SimHei", "SimSun", "DengXian", "Source Han", "Noto Sans"] {
        if text.contains(cand) {
            return Some(cand.to_string());
        }
    }
    None
}

fn font_file_for(family: &str, bold: bool) -> Option<PathBuf> {
    // 自定义字体路径优先
    if let Some(p) = custom_font_file(family) {
        return Some(p);
    }
    let dir = fonts_dir();
    if let Some((_, reg, b)) = FONT_CANDIDATES
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(family))
    {
        let pick = if bold { b } else { reg };
        let p = dir.join(pick);
        if p.exists() {
            return Some(p);
        }
        let p = dir.join(reg);
        if p.exists() {
            return Some(p);
        }
    }
    let direct = PathBuf::from(family);
    if direct.exists() {
        return Some(direct);
    }
    // 兜底：从最可靠的 CJK 字体依次尝试，保证不会因为字体名写错而整批失败
    for f in ["msyh.ttc", "simhei.ttf", "simsun.ttc", "segoeui.ttf", "arial.ttf"] {
        let p = dir.join(f);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn load_font(family: &str, bold: bool) -> Result<Arc<FontVec>, String> {
    let key = (family.to_string(), bold);
    if let Ok(cache) = font_cache().lock() {
        if let Some(f) = cache.get(&key) {
            return Ok(f.clone());
        }
    }
    let path = font_file_for(family, bold).ok_or_else(|| "系统字体目录里找不到可用字体".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("读取字体失败（{}）：{e}", path.display()))?;
    let font = FontVec::try_from_vec_and_index(bytes, 0)
        .map_err(|_| format!("字体格式不受支持：{}", path.display()))?;
    let arc = Arc::new(font);
    if let Ok(mut cache) = font_cache().lock() {
        cache.insert(key, arc.clone());
    }
    Ok(arc)
}

/// 按 Config 加载字体：优先自定义文件，其次字体名。
fn load_font_cfg(cfg: &Config) -> Result<Arc<FontVec>, String> {
    let spec = if cfg.font_path.trim().is_empty() {
        cfg.font.clone()
    } else {
        cfg.font_path.clone()
    };
    load_font(&spec, cfg.bold)
}

// ---------------- 文字 ----------------

pub fn format_taken(taken: &str, fmt: &str) -> String {
    match crate::dateinfer::parse_iso(taken) {
        Some(dt) => dt.format(fmt).to_string(),
        None => taken.chars().take(10).collect(),
    }
}

fn source_label(s: &str) -> &'static str {
    match s {
        "exif" => "EXIF",
        "video" => "视频",
        "filename" => "文件名",
        "folder" => "目录名",
        "filesystem" => "文件时间",
        "override" => "人工",
        _ => "未知",
    }
}

pub fn build_text(
    cfg: &Config,
    taken: Option<&str>,
    source: Option<&str>,
    file_name: &str,
    camera: Option<&str>,
) -> String {
    let date = taken
        .map(|t| format_taken(t, &cfg.date_format))
        .unwrap_or_else(|| "日期未知".into());
    let time = taken
        .map(|t| format_taken(t, "%H:%M"))
        .unwrap_or_else(|| "--:--".into());
    let mut out = cfg
        .text
        .replace("{date}", &date)
        .replace("{time}", &time)
        .replace("{filename}", file_name)
        .replace("{camera}", camera.unwrap_or(""));
    if cfg.source_hint && !out.is_empty() {
        out.push_str(&format!("（{}）", source_label(source.unwrap_or(""))));
    }
    if !cfg.custom.trim().is_empty() {
        out.push('\n');
        out.push_str(cfg.custom.trim());
    }
    out.replace("\r\n", "\n")
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

// ---------------- 颜色与像素 ----------------

fn parse_color(s: &str, alpha: f32) -> ([u8; 3], f32) {
    let t = s.trim().to_ascii_lowercase();
    let named = match t.as_str() {
        "white" => Some([255, 255, 255]),
        "black" => Some([0, 0, 0]),
        "red" => Some([255, 0, 0]),
        _ => None,
    };
    if let Some(c) = named {
        return (c, alpha);
    }
    let hex = t.trim_start_matches('#');
    let parse = |h: &str| u8::from_str_radix(h, 16).unwrap_or(0);
    match hex.len() {
        6 => ([parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6])], alpha),
        3 => {
            let r = parse(&hex[0..1]);
            let g = parse(&hex[1..2]);
            let b = parse(&hex[2..3]);
            ([r * 17, g * 17, b * 17], alpha)
        }
        8 => (
            [parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6])],
            (parse(&hex[6..8]) as f32 / 255.0) * alpha,
        ),
        _ => ([255, 255, 255], alpha),
    }
}

fn blend_px(dst: &mut RgbaImage, x: i32, y: i32, rgb: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= dst.width() as i32 || y >= dst.height() as i32 {
        return;
    }
    let sa = a.clamp(0.0, 1.0);
    if sa <= 0.002 {
        return;
    }
    let p = dst.get_pixel_mut(x as u32, y as u32);
    let da = p[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    for i in 0..3 {
        let v = (rgb[i] as f32 * sa + p[i] as f32 * da * (1.0 - sa)) / oa;
        p[i] = v.round().clamp(0.0, 255.0) as u8;
    }
    p[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn fill_round_rect(img: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, rgb: [u8; 3], a: f32) {
    let r = r.max(0.0).min((x1 - x0).min(y1 - y0) / 2.0);
    let xa = x0.floor().max(0.0) as i32;
    let ya = y0.floor().max(0.0) as i32;
    let xb = x1.ceil().min(img.width() as f32) as i32;
    let yb = y1.ceil().min(img.height() as f32) as i32;
    for y in ya..yb {
        for x in xa..xb {
            let cx = (x as f32 + 0.5).clamp(x0 + r, x1 - r);
            let cy = (y as f32 + 0.5).clamp(y0 + r, y1 - r);
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r * r + 0.25 {
                blend_px(img, x, y, rgb, a);
            }
        }
    }
}

// ---------------- 文字排版 ----------------

struct Placement {
    id: GlyphId,
    x: f32,
    baseline: f32,
}

fn line_width(font: &FontVec, px: f32, text: &str) -> f32 {
    let sf = font.as_scaled(PxScale::from(px));
    text.chars()
        .map(|c| sf.h_advance(font.glyph_id(c)))
        .sum::<f32>()
}

fn layout(font: &FontVec, px: f32, lines: &[String]) -> (Vec<Placement>, f32, f32) {
    let sf = font.as_scaled(PxScale::from(px));
    let ascent = sf.ascent();
    let descent = sf.descent();
    let line_h = (ascent - descent).max(px * 0.9) * 1.16;
    let widths: Vec<f32> = lines.iter().map(|l| line_width(font, px, l)).collect();
    let block_w = widths.iter().cloned().fold(0.0f32, f32::max).max(1.0);
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let mut pen = ((block_w - widths[i]) / 2.0).max(0.0);
        let baseline = ascent + i as f32 * line_h;
        for c in line.chars() {
            let id = font.glyph_id(c);
            out.push(Placement { id, x: pen, baseline });
            pen += sf.h_advance(id);
        }
    }
    (out, block_w, line_h * lines.len() as f32)
}

fn draw_placements(
    canvas: &mut RgbaImage,
    font: &FontVec,
    px: f32,
    placements: &[Placement],
    origin: (f32, f32),
    rgb: [u8; 3],
    alpha: f32,
) {
    let scale = PxScale::from(px);
    for p in placements {
        let glyph = Glyph {
            id: p.id,
            scale,
            position: ab_glyph::point(origin.0 + p.x, origin.1 + p.baseline),
        };
        let Some(og) = font.outline_glyph(glyph) else { continue };
        let b = og.px_bounds();
        og.draw(|gx, gy, cov| {
            if cov <= 0.002 {
                return;
            }
            blend_px(
                canvas,
                b.min.x as i32 + gx as i32,
                b.min.y as i32 + gy as i32,
                rgb,
                cov * alpha,
            );
        });
    }
}

fn render_block(font: &FontVec, px: f32, cfg: &Config, lines: &[String]) -> (RgbaImage, f32) {
    let (placements, block_w, block_h) = layout(font, px, lines);
    let stroke_px = (px as f64 * cfg.stroke) as f32;
    let shadow_px = (px as f64 * cfg.shadow) as f32;
    let plate_pad = if cfg.plate && !cfg.plate_full {
        (px as f64 * cfg.plate_pad) as f32
    } else {
        0.0
    };
    let pad = stroke_px.max(plate_pad).max(shadow_px).max(2.0) + 2.0;
    let w = (block_w + pad * 2.0).ceil() as u32;
    let h = (block_h + pad * 2.0).ceil() as u32;
    let mut canvas = RgbaImage::from_pixel(w.max(1), h.max(1), Rgba([0, 0, 0, 0]));

    if cfg.plate && !cfg.plate_full {
        let (rgb, a) = parse_color(&cfg.plate_color, cfg.plate_opacity as f32);
        let radius = (px as f64 * cfg.plate_radius) as f32;
        let inset = (pad - plate_pad).max(0.0);
        fill_round_rect(
            &mut canvas,
            inset,
            inset,
            w as f32 - inset,
            h as f32 - inset,
            radius,
            rgb,
            a,
        );
    }

    let text_origin = (pad, pad);
    if shadow_px > 0.0 {
        let (rgb, _) = parse_color(&cfg.shadow_color, 1.0);
        draw_placements(
            &mut canvas,
            font,
            px,
            &placements,
            (text_origin.0 + shadow_px * 0.45, text_origin.1 + shadow_px),
            rgb,
            0.45 * cfg.opacity as f32,
        );
    }
    if stroke_px > 0.5 {
        let (rgb, _) = parse_color(&cfg.stroke_color, 1.0);
        let mut dirs: Vec<(f32, f32)> = Vec::new();
        for i in 0..8 {
            let ang = std::f32::consts::TAU * i as f32 / 8.0;
            dirs.push((ang.cos() * stroke_px, ang.sin() * stroke_px));
        }
        for (dx, dy) in dirs {
            draw_placements(
                &mut canvas,
                font,
                px,
                &placements,
                (text_origin.0 + dx, text_origin.1 + dy),
                rgb,
                cfg.opacity as f32,
            );
        }
    }
    let (rgb, a) = parse_color(&cfg.color, cfg.opacity as f32);
    draw_placements(&mut canvas, font, px, &placements, text_origin, rgb, a);
    (canvas, px)
}

fn rotate_rgba(src: &RgbaImage, deg: f64) -> RgbaImage {
    let d = deg.rem_euclid(360.0);
    if d < 0.01 || (d - 360.0).abs() < 0.01 {
        return src.clone();
    }
    let rad = d.to_radians() as f32;
    let (cos, sin) = (rad.cos(), rad.sin());
    let (w, h) = (src.width() as f32, src.height() as f32);
    let nw = (w * cos.abs() + h * sin.abs()).ceil().max(1.0) as u32;
    let nh = (w * sin.abs() + h * cos.abs()).ceil().max(1.0) as u32;
    let mut out = RgbaImage::from_pixel(nw, nh, Rgba([0, 0, 0, 0]));
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (ncx, ncy) = (nw as f32 / 2.0, nh as f32 / 2.0);
    for y in 0..nh {
        for x in 0..nw {
            let dx = x as f32 + 0.5 - ncx;
            let dy = y as f32 + 0.5 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            if sx < -1.0 || sy < -1.0 || sx > w || sy > h {
                continue;
            }
            let (x0, y0) = (sx.floor().max(0.0) as u32, sy.floor().max(0.0) as u32);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let mut acc = [0.0f32; 4];
            for (ox, oy, wgt) in [
                (0u32, 0u32, (1.0 - fx) * (1.0 - fy)),
                (1, 0, fx * (1.0 - fy)),
                (0, 1, (1.0 - fx) * fy),
                (1, 1, fx * fy),
            ] {
                let px = x0 + ox;
                let py = y0 + oy;
                if px >= src.width() || py >= src.height() {
                    continue;
                }
                let p = src.get_pixel(px, py);
                for i in 0..4 {
                    acc[i] += p[i] as f32 * wgt;
                }
            }
            out.put_pixel(x, y, Rgba([
                acc[0].round().clamp(0.0, 255.0) as u8,
                acc[1].round().clamp(0.0, 255.0) as u8,
                acc[2].round().clamp(0.0, 255.0) as u8,
                acc[3].round().clamp(0.0, 255.0) as u8,
            ]));
        }
    }
    out
}

fn composite(base: &mut RgbaImage, layer: &RgbaImage, left: i32, top: i32) {
    for y in 0..layer.height() {
        for x in 0..layer.width() {
            let p = layer.get_pixel(x, y);
            if p[3] == 0 {
                continue;
            }
            let a = p[3] as f32 / 255.0;
            blend_px(base, left + x as i32, top + y as i32, [p[0], p[1], p[2]], a);
        }
    }
}

fn anchor_xy(pos: &str, img_w: u32, img_h: u32, w: u32, h: u32, margin: f32) -> (i32, i32) {
    let p = pos.to_ascii_lowercase();
    let col = p.chars().nth(1).unwrap_or('c');
    let row = p.chars().next().unwrap_or('b');
    let x = match col {
        'l' => margin,
        'r' => img_w as f32 - w as f32 - margin,
        _ => (img_w as f32 - w as f32) / 2.0,
    };
    let y = match row {
        't' => margin,
        'm' => (img_h as f32 - h as f32) / 2.0,
        _ => img_h as f32 - h as f32 - margin,
    };
    (x.round() as i32, y.round() as i32)
}

/// 自由定位：pos_x/pos_y 是水印左上角占图宽/图高的百分比。
/// 用百分比而不是像素，批量应用到不同尺寸的照片上位置关系才一致。
/// 贴边时按水印自身尺寸往里收，保证整块都在画面内。
fn free_xy(cfg: &Config, img_w: u32, img_h: u32, w: u32, h: u32) -> (i32, i32) {
    let max_x = (img_w.saturating_sub(w)) as f64;
    let max_y = (img_h.saturating_sub(h)) as f64;
    let x = (cfg.pos_x / 100.0 * img_w as f64).min(max_x).max(0.0);
    let y = (cfg.pos_y / 100.0 * img_h as f64).min(max_y).max(0.0);
    (x.round() as i32, y.round() as i32)
}

pub struct Rendered {
    pub img: RgbaImage,
    pub font_px: f32,
    /// 水印块在画面里的位置与尺寸（拖拽定位用）
    pub box_: Option<BlockBox>,
}

/// 水印块在最终画面里的位置与尺寸（像素）。
/// 拖拽定位时前端靠它把"鼠标位置"换算成"水印左上角位置"。
#[derive(Debug)]
pub struct BlockBox {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 计算水印块最终落在画面上的位置。
/// 与 `render` 用的是同一套定位逻辑，避免"预览里看到的位置"和"实际打上去的位置"不一致。
#[cfg(test)]
fn block_box(cfg: &Config, img_w: u32, img_h: u32, block_w: u32, block_h: u32) -> BlockBox {
    let short = img_w.min(img_h) as f64;
    let margin = (short * cfg.margin_pct / 100.0) as f32;
    let (x, y) = if cfg.position.eq_ignore_ascii_case("free") {
        free_xy(cfg, img_w, img_h, block_w, block_h)
    } else {
        anchor_xy(&cfg.position, img_w, img_h, block_w, block_h, margin)
    };
    BlockBox { x, y, w: block_w, h: block_h }
}

/// 画面方向校正：EXIF 缺失或方向写错时，用户可以手动把画面转正后再打水印。
pub fn rotate_image_by(img: RgbaImage, deg: i32) -> RgbaImage {
    match ((deg % 360) + 360) % 360 {
        90 => image::imageops::rotate90(&img),
        180 => image::imageops::rotate180(&img),
        270 => image::imageops::rotate270(&img),
        _ => img,
    }
}

/// 在已按方向校正的图像上绘制水印。
pub fn render_for_asset(conn: &rusqlite::Connection, id: &str, src: &Path, img: RgbaImage, cfg: &Config) -> Result<RgbaImage, String> {
    let (taken, source, camera): (Option<String>,Option<String>,Option<String>) = conn.query_row(
        "SELECT effective_taken_at,date_source,camera FROM media_assets WHERE id=?1", rusqlite::params![id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;
    if cfg.require_date && cfg.text.contains("{date}") && taken.is_none() {
        return Err("没有拍摄日期，无法添加日期水印；请先指定日期或关闭水印页的日期要求".into());
    }
    let text = build_text(cfg,taken.as_deref(),source.as_deref(),&crate::util::file_name_of(src),camera.as_deref());
    let rendered = render(rotate_image_by(img,cfg.rotate_image),cfg,&text);
    if rendered.font_px <= 0.0 { return Err("没有可绘制的水印文字，请检查文字与字体设置".into()); }
    Ok(rendered.img)
}

pub fn render(img: RgbaImage, cfg: &Config, text: &str) -> Rendered {
    let cfg = Config::clean(cfg.clone());
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Rendered { img, font_px: 0.0, box_: None };
    }
    let font = match load_font_cfg(&cfg) {
        Ok(f) => f,
        Err(_) => return Rendered { img, font_px: 0.0, box_: None },
    };
    let (w, h) = (img.width().max(1), img.height().max(1));
    let short = w.min(h) as f64;
    let mut px = (short * cfg.size_pct / 100.0).clamp(cfg.min_px, cfg.max_px) as f32;
    // 宽度保护：超过上限就整体缩小字号（不裁切、不换行会破坏日期语义）
    let max_w = w as f32 * (cfg.max_width_pct as f32) / 100.0;
    let block_w = lines
        .iter()
        .map(|l| line_width(&font, px, l))
        .fold(0.0f32, f32::max);
    if max_w > 1.0 && block_w > max_w {
        px = (px * max_w / block_w).max(6.0);
    }
    // 高度保护：多行文字最多占画面 45%
    let approx_h = px * 1.16 * lines.len() as f32;
    if approx_h > h as f32 * 0.45 {
        px = (px * (h as f32 * 0.45) / approx_h).max(6.0);
    }

    let (block, used_px) = render_block(&font, px, &cfg, &lines);
    let block = if cfg.rotate.abs() > 0.01 {
        rotate_rgba(&block, cfg.rotate)
    } else {
        block
    };
    let margin = (short * cfg.margin_pct / 100.0) as f32;
    let mut out = img;
    let is_free = cfg.position.eq_ignore_ascii_case("free");
    let used_box: Option<BlockBox>;
    if cfg.plate && cfg.plate_full {
        // 整幅底栏：通栏压在画面上下边缘，文字居中放在栏内
        let pad_px = (px as f64 * cfg.plate_pad) as f32;
        let band_h = block.height() as f32 + pad_px * 2.0;
        let row = cfg.position.chars().next().unwrap_or('b');
        let y0 = match row {
            't' => 0.0,
            'm' => (h as f32 - band_h) / 2.0,
            _ => h as f32 - band_h,
        };
        let (rgb, a) = parse_color(&cfg.plate_color, cfg.plate_opacity as f32);
        fill_round_rect(&mut out, 0.0, y0, w as f32, y0 + band_h, 0.0, rgb, a);
        let (x, _) = if is_free {
            free_xy(&cfg, w, h, block.width(), block.height())
        } else {
            anchor_xy(&cfg.position, w, h, block.width(), block.height(), margin)
        };
        let ty = (y0 + pad_px).round() as i32;
        composite(&mut out, &block, x, ty);
        used_box = Some(BlockBox { x, y: ty, w: block.width(), h: block.height() });
    } else {
        let (x, y) = if is_free {
            free_xy(&cfg, w, h, block.width(), block.height())
        } else {
            anchor_xy(&cfg.position, w, h, block.width(), block.height(), margin)
        };
        composite(&mut out, &block, x, y);
        used_box = Some(BlockBox { x, y, w: block.width(), h: block.height() });
    }
    Rendered { img: out, font_px: used_px, box_: used_box }
}

// ---------------- 编码与 EXIF ----------------

/// 按扩展名编码。png/webp/jpg/tif 各走各自的编码器，
/// 绝不能让 TIFF 落到 JPEG 分支——那样写出来的字节和扩展名对不上，
/// 看图软件会当成损坏文件（覆盖模式下就是直接毁掉原图）。
pub fn encode(img: &RgbaImage, ext: &str, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
    match ext {
        "png" => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgba8();
            image::DynamicImage::ImageRgba8(rgb)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .map_err(|e| format!("PNG 编码失败：{e}"))?;
        }
        "webp" => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut buf);
            enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("WebP 编码失败：{e}"))?;
        }
        "tif" | "tiff" => {
            // TIFF 是有损的格式转换（源像素已带 alpha），统一转成 RGB8 再写，
            // 与 JPEG 分支保持一致；否则 RGBA 存进 TIFF 会让部分看图软件打不开。
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            image::DynamicImage::ImageRgb8(rgb)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Tiff)
                .map_err(|e| format!("TIFF 编码失败：{e}"))?;
        }
        _ => {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let mut enc =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(40, 100));
            enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("JPEG 编码失败：{e}"))?;
        }
    }
    Ok(buf)
}

fn target_ext(cfg: &Config, src_ext: &str) -> String {
    match cfg.output_format.as_str() {
        "jpg" | "jpeg" => "jpg".into(),
        "png" => "png".into(),
        "webp" => "webp".into(),
        // "保持原格式"：能原样写回的格式就原样保留扩展名。
        // TIFF 必须保留，否则覆盖模式会因为"格式与原文件不一致"被误拒。
        _ => match src_ext {
            "png" | "webp" | "tif" | "tiff" => src_ext.to_string(),
            _ => "jpg".into(),
        },
    }
}

/// HEIC/HEIF 里的 EXIF 是裸 TIFF 块，需要自己包成 APP1 段。
fn heic_exif_segment(src: &Path) -> Option<Vec<u8>> {
    let tiff = crate::heif::exif_tiff(src)?;
    if tiff.len() < 8 {
        return None;
    }
    let len = (2 + 6 + tiff.len()) as u16;
    let mut seg = Vec::with_capacity(len as usize + 2);
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&len.to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    reset_orientation(&mut seg);
    Some(seg)
}

/// 从源文件里取出 APP1(Exif) 段，并把 Orientation 改成 1（像素已经转过）。
fn exif_segment(src: &Path) -> Option<Vec<u8>> {
    let ext = util::ext_of(src);
    if matches!(ext.as_str(), "heic" | "heif") {
        return heic_exif_segment(src);
    }
    let data = std::fs::read(src).ok()?;
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
        if len < 2 || i + 2 + len > data.len() {
            break;
        }
        if marker == 0xE1 && data[i + 4..(i + 10).min(data.len())] == *b"Exif\0\0" {
            let mut seg = data[i..i + 2 + len].to_vec();
            reset_orientation(&mut seg);
            return Some(seg);
        }
        i += 2 + len;
    }
    None
}

/// 给编辑模块用：取出 APP1(Exif) 段并把 Orientation 归 1。
pub fn exif_segment_public(src: &Path) -> Option<Vec<u8>> {
    exif_segment(src)
}

/// 把 APP1 里的 Orientation 改成 1。像素已经按方向旋转过，
/// 若原样保留 Orientation=6/8，看图软件会再转一次。
fn reset_orientation(seg: &mut [u8]) {
    // 段结构：FFE1 len "Exif\0\0" TIFF header
    let tiff = 10usize;
    if seg.len() < tiff + 8 {
        return;
    }
    let little = match &seg[tiff..tiff + 2] {
        b"II" => true,
        b"MM" => false,
        _ => return,
    };
    let read16 = |b: &[u8], o: usize| -> u16 {
        if little {
            u16::from_le_bytes([b[o], b[o + 1]])
        } else {
            u16::from_be_bytes([b[o], b[o + 1]])
        }
    };
    let read32 = |b: &[u8], o: usize| -> u32 {
        if little {
            u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
        } else {
            u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
        }
    };
    let ifd_off = read32(seg, tiff + 4) as usize;
    let ifd = tiff + ifd_off;
    if ifd + 2 > seg.len() {
        return;
    }
    let count = read16(seg, ifd) as usize;
    for i in 0..count {
        let e = ifd + 2 + i * 12;
        if e + 12 > seg.len() {
            return;
        }
        if read16(seg, e) == 0x0112 {
            // 值内联在 entry 的第 8..10 字节
            if little {
                seg[e + 8..e + 10].copy_from_slice(&1u16.to_le_bytes());
            } else {
                seg[e + 8..e + 10].copy_from_slice(&1u16.to_be_bytes());
            }
            return;
        }
    }
}

fn splice_exif(mut jpeg: Vec<u8>, src: &Path, keep: bool) -> Vec<u8> {
    if !keep {
        return jpeg;
    }
    let Some(seg) = exif_segment(src) else { return jpeg };
    if jpeg.len() < 2 {
        return jpeg;
    }
    let mut out = Vec::with_capacity(jpeg.len() + seg.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[2..]);
    jpeg.clear();
    out
}

// ---------------- 单图预览 ----------------

pub fn preview(db_path: &Path, asset_id: &str, cfg: &Config, max_side: u32) -> Result<Value, String> {
    let conn = crate::db::open(db_path)?;
    let cfg = Config::clean(cfg.clone());
    let (taken, source, camera): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT effective_taken_at, date_source, camera FROM media_assets WHERE id=?1",
            rusqlite::params![asset_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| format!("找不到该条目：{e}"))?;
    let file = assets::primary_file(&conn, asset_id)?
        .ok_or_else(|| "该条目没有可用文件".to_string())?;
    if file.availability != "online" {
        return Err("原文件当前不可用（外置盘离线或已移动）".into());
    }
    let path = PathBuf::from(&file.absolute_path);
    let mut img = media::load_oriented(&path)?.to_rgba8();
    img = rotate_image_by(img, cfg.rotate_image);
    if max_side > 0 && (img.width() > max_side || img.height() > max_side) {
        let ratio = max_side as f32 / img.width().max(img.height()) as f32;
        let nw = ((img.width() as f32 * ratio).round() as u32).max(1);
        let nh = ((img.height() as f32 * ratio).round() as u32).max(1);
        img = image::imageops::resize(&img, nw, nh, FilterType::Lanczos3);
    }
    let text = build_text(&cfg, taken.as_deref(), source.as_deref(), &file.file_name, camera.as_deref());
    let mut warning = String::new();
    if text.is_empty() || (taken.is_none() && cfg.require_date && cfg.text.contains("{date}")) {
        warning = "这张照片没有可靠的拍摄时间，批量执行时会被跳过（可在「待整理」里先指定日期）".into();
    }
    let rendered = render(img, &cfg, &text);
    let bytes = encode(&rendered.img, "jpg", 88)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let bx = rendered.box_.as_ref().map(|b| {
        json!({ "x": b.x, "y": b.y, "w": b.w, "h": b.h })
    });
    Ok(json!({
        "image": format!("data:image/jpeg;base64,{b64}"),
        "width": rendered.img.width(),
        "height": rendered.img.height(),
        "font_px": rendered.font_px.round(),
        "text": text,
        "taken_at": taken,
        "date_source": source,
        "file_name": file.file_name,
        // 原始 EXIF 方向：界面用它提示"这张是竖拍的、按 6 转过来了"
        "exif_orientation": media::image_orientation(&path),
        "rotate_image": cfg.rotate_image,
        "block": bx,
        "pos_x": cfg.pos_x,
        "pos_y": cfg.pos_y,
        "warning": warning,
    }))
}

// ---------------- 批量执行 ----------------

#[derive(Deserialize)]
#[serde(default)]
pub struct RunRequest {
    pub ids: Vec<String>,
    pub out_dir: String,
    pub config: Config,
    pub name_template: String,
    pub quality: u8,
    pub keep_exif: bool,
    pub skip_existing: bool,
    /// copy（默认，输出新副本）| overwrite（直接覆盖原图）
    pub mode: String,
    /// 覆盖前把原件备份到 backups/<job>/；默认开启
    pub backup: bool,
}

impl Default for RunRequest {
    fn default() -> Self {
        RunRequest {
            ids: Vec::new(),
            out_dir: String::new(),
            config: Config::default(),
            name_template: "{name}_wm.{ext}".into(),
            quality: 92,
            keep_exif: true,
            skip_existing: true,
            mode: "copy".into(),
            backup: true,
        }
    }
}

/// 把水印配置换算成 ffmpeg 的文字样式。
fn video_style(cfg: &Config) -> crate::ffmpeg::TextStyle {
    // 自定义字体文件优先，其次按字体名去系统目录找
    let family = if cfg.font_path.trim().is_empty() {
        cfg.font.clone()
    } else {
        cfg.font_path.clone()
    };
    let font = crate::ffmpeg::font_file(&family, cfg.bold)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows\\Fonts\\msyh.ttc"));
    crate::ffmpeg::TextStyle {
        font,
        size_ratio: (cfg.size_pct / 100.0) as f32,
        color: cfg.color.clone(),
        opacity: cfg.opacity as f32,
        stroke: cfg.stroke as f32,
        stroke_color: cfg.stroke_color.clone(),
        shadow: cfg.shadow as f32,
        boxed: cfg.plate,
        box_color: cfg.plate_color.clone(),
        box_opacity: cfg.plate_opacity as f32,
        position: cfg.position.clone(),
        free_x: cfg.pos_x as f32,
        free_y: cfg.pos_y as f32,
        margin: (cfg.margin_pct / 100.0) as f32,
    }
}

/// 记录一条可还原的备份（覆盖模式）。返回是否写入成功。
fn record_backup(
    conn: &rusqlite::Connection,
    job_id: &str,
    asset_id: &str,
    file_path: &str,
    backup: Option<&Path>,
) -> bool {
    let Some(bp) = backup else { return false };
    let bytes = std::fs::metadata(bp).map(|m| m.len() as i64).unwrap_or(0);
    conn.execute(
        "INSERT INTO watermark_backups(job_id,asset_id,file_path,backup_path,bytes,state,created_at)
         VALUES(?1,?2,?3,?4,?5,'done',?6)",
        rusqlite::params![job_id, asset_id, file_path, bp.to_string_lossy(), bytes, crate::util::now_iso()],
    )
    .is_ok()
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for i in 2..1000 {
        let name = if ext.is_empty() {
            format!("{stem} ({i})")
        } else {
            format!("{stem} ({i}).{ext}")
        };
        let p = parent.join(name);
        if !p.exists() {
            return p;
        }
    }
    path.to_path_buf()
}

pub fn run(db_path: &Path, job: &JobCtl, req: RunRequest) -> Result<(), String> {
    let overwrite = req.mode.eq_ignore_ascii_case("overwrite");
    let out_root = req.out_dir.trim();
    if !overwrite && out_root.is_empty() {
        return Err("没有指定输出目录".into());
    }
    let out_root = if out_root.is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(out_root)
    };
    if !overwrite {
        std::fs::create_dir_all(&out_root).map_err(|e| format!("无法创建输出目录：{e}"))?;
    }
    // 覆盖模式的备份目录：数据目录\backups\wm-<jobId>，真正用到时才创建
    let backup_root = if overwrite && req.backup {
        Some(
            db_path
                .parent()
                .unwrap_or(Path::new("."))
                .join("backups")
                .join(format!("wm-{}", job.id)),
        )
    } else {
        None
    };
    let cfg = Config::clean(req.config.clone());
    let conn = crate::db::open(db_path)?;
    let quality = req.quality.clamp(40, 100);

    let mut errors: Vec<String> = Vec::new();
    let mut skipped_nodate = 0;
    let mut overwritten = 0usize;
    let mut restorable = 0usize;
    let mut skipped_video = 0usize;
    let mut no_ffmpeg = false;
    job.set_total(req.ids.len() as i64);
    job.phase(if overwrite { "准备覆盖水印…" } else { "准备水印任务…" });

    for (idx, id) in req.ids.iter().enumerate() {
        if job.is_cancelled() {
            break;
        }
        let row: Option<(Option<String>, Option<String>, Option<String>, String)> = conn
            .query_row(
                "SELECT effective_taken_at, date_source, camera, media_type FROM media_assets WHERE id=?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((taken, source, camera, media_type)) = row else {
            job.fail();
            if errors.len() < 300 {
                errors.push(format!("{id}：索引里找不到该条目"));
            }
            continue;
        };
        let file = match assets::primary_file(&conn, id) {
            Ok(Some(f)) => f,
            Ok(None) => {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{id}：没有可用文件"));
                }
                continue;
            }
            Err(e) => {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{id}：{e}"));
                }
                continue;
            }
        };
        if file.availability != "online" {
            job.fail();
            if errors.len() < 300 {
                errors.push(format!("{}：原文件不可用", file.file_name));
            }
            continue;
        }
        let needs_date = cfg.require_date && cfg.text.contains("{date}") && taken.is_none();
        if needs_date {
            skipped_nodate += 1;
            job.skip();
            continue;
        }
        let text = build_text(&cfg, taken.as_deref(), source.as_deref(), &file.file_name, camera.as_deref());
        let src = PathBuf::from(&file.absolute_path);

        // ---- 视频：交给外部 ffmpeg（程序自带编码器不现实，见 TODO.md）----
        if media_type == "video" {
            if !cfg.video {
                skipped_video += 1;
                job.skip();
                continue;
            }
            if crate::ffmpeg::locate().is_none() {
                skipped_video += 1;
                if errors.len() < 300 {
                    no_ffmpeg = true;
                }
                job.skip();
                continue;
            }
            let src_ext = file.ext.to_lowercase();
            let target = if overwrite {
                src.clone()
            } else {
                let rel = crate::archive::render_rel_path(
                    &file.file_name,
                    &src_ext,
                    "video",
                    camera.as_deref(),
                    taken.as_deref(),
                    &req.name_template,
                );
                let mut t = out_root.join(rel.replace('\\', std::path::MAIN_SEPARATOR_STR));
                if let Some(parent) = t.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        job.fail();
                        if errors.len() < 300 {
                            errors.push(format!("{}：无法创建目录 {e}", file.file_name));
                        }
                        continue;
                    }
                }
                if t.exists() {
                    if req.skip_existing {
                        job.skip();
                        continue;
                    }
                    t = unique_path(&t);
                }
                t
            };
            let style = video_style(&cfg);
            let mut backup_used: Option<PathBuf> = None;
            let result = (|| -> Result<(), String> {
                if overwrite {
                    if let Some(root) = &backup_root {
                        std::fs::create_dir_all(root).map_err(|e| format!("无法创建备份目录：{e}"))?;
                        let dst = unique_path(&root.join(&file.file_name));
                        std::fs::copy(&src, &dst).map_err(|e| format!("备份原件失败，已放弃覆盖：{e}"))?;
                        backup_used = Some(dst);
                    }
                }
                crate::ffmpeg::watermark_video(&src, &target, &style, &text, quality)
            })();
            match result {
                Ok(()) => {
                    job.ok();
                    if overwrite {
                        overwritten += 1;
                        refresh_after_overwrite(&conn, &file, db_path)?;
                        if record_backup(&conn, &job.id, &file.asset_id, &file.absolute_path, backup_used.as_deref()) {
                            restorable += 1;
                        }
                        crate::db::log_op(
                            &conn,
                            "watermark",
                            Some(&file.absolute_path),
                            Some(&file.absolute_path),
                            None,
                            None,
                            "ok",
                            Some(&job.id),
                            Some(&format!("视频水印覆盖：{}", text.replace('\n', " / "))),
                        );
                    } else {
                        crate::scan::register_output(&conn, id, &job.id, "watermark", &target)?;
                        crate::db::log_op(
                            &conn,
                            "watermark",
                            Some(&file.absolute_path),
                            Some(&target.to_string_lossy()),
                            None,
                            None,
                            "ok",
                            Some(&job.id),
                            Some(&format!("视频水印：{}", text.replace('\n', " / "))),
                        );
                    }
                }
                Err(e) => {
                    job.fail();
                    if errors.len() < 300 {
                        errors.push(format!("{}：{e}", file.file_name));
                    }
                }
            }
            if idx % 3 == 0 {
                job.phase(&format!("正在处理视频… {}/{}", idx + 1, req.ids.len()));
            }
            continue;
        }

        let src_ext = file.ext.to_lowercase();
        let (target, ext, is_overwrite) = if overwrite {
            // 覆盖原图时格式必须与原文件一致：文件名不变、内容换格式会让看图软件认错
            if matches!(src_ext.as_str(), "heic" | "heif") {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{}：HEIC 无法原地覆盖，请改用「新建副本」并把输出格式设为 JPEG", file.file_name));
                }
                continue;
            }
            if !matches!(src_ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "tif" | "tiff") {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{}：该格式不支持原地覆盖", file.file_name));
                }
                continue;
            }
            let picked = target_ext(&cfg, &src_ext);
            let same = picked == src_ext || (picked == "jpg" && src_ext == "jpeg");
            if !same {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!(
                        "{}：覆盖原图时输出格式必须与原文件相同（当前选了 {}）",
                        file.file_name, cfg.output_format
                    ));
                }
                continue;
            }
            (src.clone(), src_ext.clone(), true)
        } else {
            let ext = target_ext(&cfg, &file.ext);
            let rel = crate::archive::render_rel_path(
                &file.file_name,
                &ext,
                "image",
                camera.as_deref(),
                taken.as_deref(),
                &req.name_template,
            );
            let mut target = out_root.join(rel.replace('\\', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    job.fail();
                    if errors.len() < 300 {
                        errors.push(format!("{}：无法创建目录 {e}", file.file_name));
                    }
                    continue;
                }
            }
            if target.exists() {
                if req.skip_existing {
                    job.skip();
                    continue;
                }
                target = unique_path(&target);
            }
            (target, ext, false)
        };
        let mut backup_used: Option<PathBuf> = None;
        let result = (|| -> Result<(), String> {
            // 覆盖前先把原件搬到备份目录（失败就不动原图）
            if is_overwrite {
                if let Some(root) = &backup_root {
                    std::fs::create_dir_all(root).map_err(|e| format!("无法创建备份目录：{e}"))?;
                    let dst = unique_path(&root.join(&file.file_name));
                    std::fs::copy(&src, &dst).map_err(|e| format!("备份原件失败，已放弃覆盖：{e}"))?;
                    backup_used = Some(dst);
                }
            }
            let img = media::load_oriented(&src)?.to_rgba8();
            let img = rotate_image_by(img, cfg.rotate_image);
            let rendered = render(img, &cfg, &text);
            if rendered.font_px <= 0.0 {
                return Err("没有可绘制的文字（检查字体是否可用）".into());
            }
            let mut bytes = encode(&rendered.img, &ext, quality)?;
            if ext == "jpg" || ext == "jpeg" {
                bytes = splice_exif(bytes, &src, req.keep_exif);
            }
            if is_overwrite {
                // 先写临时文件再原子替换，避免中途失败把原图写坏
                let tmp = src.with_file_name(format!(".{}.pvm-tmp", file.file_name));
                std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时文件失败：{e}"))?;
                std::fs::rename(&tmp, &src).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    format!("替换原文件失败：{e}")
                })?;
            } else {
                std::fs::write(&target, &bytes).map_err(|e| format!("写入失败：{e}"))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                job.ok();
                if is_overwrite {
                    overwritten += 1;
                    refresh_after_overwrite(&conn, &file, db_path)?;
                    if record_backup(&conn, &job.id, &file.asset_id, &file.absolute_path, backup_used.as_deref()) {
                        restorable += 1;
                    }
                    crate::db::log_op(
                        &conn,
                        "watermark",
                        Some(&file.absolute_path),
                        Some(&file.absolute_path),
                        None,
                        None,
                        "ok",
                        Some(&job.id),
                        Some(&format!("水印覆盖原图：{}", text.replace('\n', " / "))),
                    );
                } else {
                    crate::scan::register_output(&conn, id, &job.id, "watermark", &target)?;
                    crate::db::log_op(
                        &conn,
                        "watermark",
                        Some(&file.absolute_path),
                        Some(&target.to_string_lossy()),
                        None,
                        None,
                        "ok",
                        Some(&job.id),
                        Some(&format!("水印：{}", text.replace('\n', " / "))),
                    );
                }
            }
            Err(e) => {
                job.fail();
                if errors.len() < 300 {
                    errors.push(format!("{}：{e}", file.file_name));
                }
            }
        }
        if idx % 5 == 0 {
            job.phase(&format!("正在生成水印… {}/{}", idx + 1, req.ids.len()));
        }
    }

    let p = job.snapshot();
    let mut msg = format!(
        "成功 {}（其中覆盖原图 {}），跳过 {}（其中无拍摄时间 {}），失败 {}",
        p.succeeded, overwritten, p.skipped, skipped_nodate, p.failed
    );
    if let Some(root) = &backup_root {
        if root.exists() {
            msg.push_str(&format!("；原件备份：{}", root.display()));
        }
    }
    if restorable > 0 {
        msg.push_str(&format!("；其中 {} 张可在「覆盖记录」里一键还原", restorable));
    }
    if skipped_video > 0 {
        if no_ffmpeg {
            msg.push_str(&format!("；跳过视频 {}（未检测到 ffmpeg.exe，视频水印不可用）", skipped_video));
        } else {
            msg.push_str(&format!("；跳过视频 {}", skipped_video));
        }
    }
    if let Some(first) = errors.first() {
        msg.push_str(&format!("；首个问题：{first}"));
    }
    job.phase(&msg);
    Ok(())
}

/// 覆盖原图后：更新尺寸/时间戳并清掉旧缩略图缓存，否则界面还在显示打水印前的图。
fn refresh_after_overwrite(conn: &rusqlite::Connection, file: &assets::FileRow, _db_path: &Path) -> Result<(), String> {
    crate::scan::refresh_modified(conn, Path::new(&file.absolute_path))
}

// ---------------- 覆盖备份：列表 / 还原 / 清理 ----------------

/// 清掉某条目的缩略图缓存（还原后必须清，否则界面还显示带水印的图）。
fn clear_previews(conn: &rusqlite::Connection, asset_id: &str) {
    if let Ok(mut st) = conn.prepare("SELECT cache_path FROM media_previews WHERE asset_id=?1") {
        if let Ok(rows) = st.query_map(rusqlite::params![asset_id], |r| r.get::<_, String>(0)) {
            for p in rows.flatten() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let _ = conn.execute(
        "DELETE FROM media_previews WHERE asset_id=?1",
        rusqlite::params![asset_id],
    );
}

/// 覆盖批次列表：给界面上的「覆盖记录」用。
pub fn backup_batches(db_path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let conn = crate::db::open(db_path)?;
    let mut st = conn
        .prepare(
            "SELECT job_id,
                    COUNT(*) AS total,
                    SUM(CASE WHEN state='done' THEN 1 ELSE 0 END) AS pending,
                    SUM(CASE WHEN state='restored' THEN 1 ELSE 0 END) AS restored,
                    SUM(CASE WHEN state='missing' THEN 1 ELSE 0 END) AS missing,
                    SUM(bytes) AS bytes,
                    MIN(created_at) AS created_at
             FROM watermark_backups
             GROUP BY job_id
             ORDER BY MIN(id) DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (job_id, total, pending, restored, missing, bytes, created_at) = row;
        let samples: Vec<String> = {
            let mut s = conn
                .prepare("SELECT file_path FROM watermark_backups WHERE job_id=?1 ORDER BY id LIMIT 3")
                .map_err(|e| e.to_string())?;
            let rows: Vec<String> = s
                .query_map(rusqlite::params![job_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            rows.into_iter()
                .map(|p| {
                    Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or(p)
                })
                .collect()
        };
        out.push(serde_json::json!({
            "job_id": job_id,
            "total": total,
            "pending": pending,
            "restored": restored,
            "missing": missing,
            "bytes": bytes,
            "created_at": created_at.unwrap_or_default(),
            "samples": samples,
        }));
    }
    Ok(out)
}

/// 还原一个覆盖批次：把备份文件写回原位置。返回 (成功, 失败)。
pub fn restore_batch(db_path: &Path, job_id: &str) -> Result<(usize, usize, Vec<String>), String> {
    let conn = crate::db::open(db_path)?;
    let mut st = conn
        .prepare(
            "SELECT id, asset_id, file_path, backup_path FROM watermark_backups
             WHERE job_id=?1 AND state='done' ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(i64, String, String, String)> = st
        .query_map(rusqlite::params![job_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    if rows.is_empty() {
        return Err("这个批次没有可还原的文件".into());
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for (row_id, asset_id, file_path, backup_path) in rows {
        let src = PathBuf::from(&backup_path);
        let dst = PathBuf::from(&file_path);
        let name = dst
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.clone());
        if !src.exists() {
            let _ = conn.execute(
                "UPDATE watermark_backups SET state='missing' WHERE id=?1",
                rusqlite::params![row_id],
            );
            failed += 1;
            if errors.len() < 50 {
                errors.push(format!("{name}：备份文件已不存在"));
            }
            continue;
        }
        let write = (|| -> Result<(), String> {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("无法创建目录：{e}"))?;
            }
            let tmp = dst.with_file_name(format!(".{name}.pvm-restore"));
            std::fs::copy(&src, &tmp).map_err(|e| format!("写入失败：{e}"))?;
            std::fs::rename(&tmp, &dst).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("替换失败：{e}")
            })?;
            Ok(())
        })();
        match write {
            Ok(()) => {
                ok += 1;
                let meta = std::fs::metadata(&dst).ok();
                let size = meta.as_ref().map(|m| m.len() as i64).unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let _ = conn.execute(
                    "UPDATE media_files SET size_bytes=?1, mtime_ms=?2 WHERE absolute_path=?3",
                    rusqlite::params![size, mtime, file_path],
                );
                clear_previews(&conn, &asset_id);
                crate::scan::refresh_modified(&conn, &dst)?;
                let _ = conn.execute(
                    "UPDATE watermark_backups SET state='restored', restored_at=?1 WHERE id=?2",
                    rusqlite::params![crate::util::now_iso(), row_id],
                );
                crate::db::log_op(
                    &conn,
                    "watermark-restore",
                    Some(&file_path),
                    Some(&file_path),
                    None,
                    None,
                    "ok",
                    Some(job_id),
                    Some("还原覆盖前的水印原件"),
                );
            }
            Err(e) => {
                failed += 1;
                if errors.len() < 50 {
                    errors.push(format!("{name}：{e}"));
                }
            }
        }
    }
    Ok((ok, failed, errors))
}

/// 删除一个批次的备份（放弃还原）。返回删除的文件数。
pub fn drop_batch(db_path: &Path, job_id: &str) -> Result<usize, String> {
    let conn = crate::db::open(db_path)?;
    let mut st = conn
        .prepare("SELECT backup_path FROM watermark_backups WHERE job_id=?1")
        .map_err(|e| e.to_string())?;
    let files: Vec<String> = st
        .query_map(rusqlite::params![job_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let mut removed = 0usize;
    for f in &files {
        if std::fs::remove_file(f).is_ok() {
            removed += 1;
        }
    }
    let _ = conn.execute(
        "DELETE FROM watermark_backups WHERE job_id=?1",
        rusqlite::params![job_id],
    );
    // 目录空了就一并删掉
    if let Some(parent) = files.first().and_then(|f| Path::new(f).parent()) {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(removed)
}

// ---------------- 预设 ----------------

pub fn builtin_presets() -> Vec<(&'static str, &'static str, Config)> {
    let classic = Config {
        position: "br".into(),
        size_pct: 3.2,
        bold: true,
        font: "微软雅黑".into(),
        color: "#FF8A1E".into(),
        stroke: 0.055,
        stroke_color: "#20120A".into(),
        shadow: 0.11,
        date_format: "%Y.%m.%d %H:%M".into(),
        ..Config::default()
    };
    let clean = Config {
        position: "br".into(),
        size_pct: 2.3,
        bold: false,
        color: "#FFFFFF".into(),
        opacity: 0.95,
        stroke: 0.03,
        stroke_color: "#000000".into(),
        shadow: 0.0,
        plate: false,
        date_format: "%Y-%m-%d".into(),
        ..Config::default()
    };
    let frame = Config {
        position: "bc".into(),
        size_pct: 3.0,
        margin_pct: 0.0,
        bold: false,
        color: "#F2F2F2".into(),
        stroke: 0.0,
        shadow: 0.0,
        plate: true,
        plate_color: "#000000".into(),
        plate_opacity: 0.6,
        plate_radius: 0.0,
        plate_pad: 0.55,
        plate_full: true,
        date_format: "%Y-%m-%d  %H:%M".into(),
        ..Config::default()
    };
    vec![
        ("wm-classic", "经典数码橙", classic),
        ("wm-clean", "简约白字", clean),
        ("wm-frame", "相框底栏", frame),
    ]
}

pub fn seed_presets(conn: &rusqlite::Connection) -> Result<(), String> {
    for (id, name, cfg) in builtin_presets() {
        let json = serde_json::to_string(&cfg).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO watermark_presets(id,name,config_json,builtin,created_at)
             VALUES(?1,?2,?3,1,?4)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, config_json=excluded.config_json
             WHERE watermark_presets.builtin=1",
            rusqlite::params![id, name, json, util::now_iso()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn presets_list(conn: &rusqlite::Connection) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id,name,config_json,builtin FROM watermark_presets
             ORDER BY builtin DESC, name COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let json: String = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "builtin": r.get::<_, i64>(3)? != 0,
                "config": serde_json::from_str::<Value>(&json).unwrap_or(Value::Null),
            }))
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({ "presets": rows.filter_map(|r| r.ok()).collect::<Vec<_>>() }))
}

pub fn preset_save(conn: &rusqlite::Connection, name: &str, cfg: &Config) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("预设名称不能为空".into());
    }
    let id = format!("wm-{}", util::uuid());
    let json = serde_json::to_string(&Config::clean(cfg.clone())).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO watermark_presets(id,name,config_json,builtin,created_at) VALUES(?1,?2,?3,0,?4)",
        rusqlite::params![id, name, json, util::now_iso()],
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({ "id": id, "name": name }))
}

pub fn preset_delete(conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    let builtin: Option<i64> = conn
        .query_row(
            "SELECT builtin FROM watermark_presets WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .ok();
    match builtin {
        None => Err("预设不存在".into()),
        Some(1) => Err("内置预设不能删除".into()),
        Some(_) => {
            conn.execute("DELETE FROM watermark_presets WHERE id=?1", rusqlite::params![id])
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_tiff_produces_real_tiff_bytes() {
        // 回归测试：TIFF 曾经落到 JPEG 分支，写出"扩展名是 .tif、内容是 JPEG"的坏文件。
        let mut img = RgbaImage::new(8, 4);
        for p in img.pixels_mut() {
            p.0 = [10, 20, 30, 255];
        }
        let bytes = encode(&img, "tif", 92).expect("TIFF 编码失败");
        assert!(bytes.len() > 8, "TIFF 数据为空");
        // TIFF 文件头：II*\0（小端）或 MM\0*（大端）
        let little = &bytes[..4] == [0x49, 0x49, 0x2A, 0x00];
        let big = &bytes[..4] == [0x4D, 0x4D, 0x00, 0x2A];
        assert!(little || big, "TIFF 文件头不对：{:?}", &bytes[..4]);
        // 也不能是 JPEG（FF D8 FF）
        assert!(bytes[0] != 0xFF || bytes[1] != 0xD8, "TIFF 被误写成 JPEG");
        // 编码出来的 TIFF 必须能被解码回来
        let back = image::load_from_memory(&bytes).expect("TIFF 无法解码");
        assert_eq!((back.width(), back.height()), (8, 4));
    }

    #[test]
    fn encode_tiff_alias_also_writes_tiff() {
        let img = RgbaImage::new(4, 4);
        let a = encode(&img, "tif", 92).unwrap();
        let b = encode(&img, "tiff", 92).unwrap();
        assert_eq!(&a[..4], &b[..4]);
        assert!(a[..4] == [0x49, 0x49, 0x2A, 0x00] || a[..4] == [0x4D, 0x4D, 0x00, 0x2A]);
    }

    #[test]
    fn target_ext_keeps_tiff_when_format_is_same() {
        // "保持原格式"必须真的保持 TIFF，否则覆盖模式会因"格式不一致"被误拒
        let cfg = Config {
            output_format: "same".into(),
            ..Config::default()
        };
        assert_eq!(target_ext(&cfg, "tif"), "tif");
        assert_eq!(target_ext(&cfg, "tiff"), "tiff");
        assert_eq!(target_ext(&cfg, "png"), "png");
        assert_eq!(target_ext(&cfg, "webp"), "webp");
        // 其它格式（如 HEIC）无法写回，降级为 JPEG
        assert_eq!(target_ext(&cfg, "heic"), "jpg");
        // 显式指定格式时按指定走
        let cfg2 = Config {
            output_format: "png".into(),
            ..Config::default()
        };
        assert_eq!(target_ext(&cfg2, "tif"), "png");
    }

    #[test]
    fn color_parse() {
        assert_eq!(parse_color("#FF8A1E", 1.0).0, [255, 138, 30]);
        assert_eq!(parse_color("#fff", 1.0).0, [255, 255, 255]);
        let (_, a) = parse_color("#000000", 0.5);
        assert!((a - 0.5).abs() < 1e-6);
    }

    #[test]
    fn date_format_tokens() {
        assert_eq!(format_taken("2023-05-01T14:25:30", "%Y年%m月%d日"), "2023年05月01日");
        assert_eq!(format_taken("2023-05-01T14:25:30", "%H:%M"), "14:25");
    }

    #[test]
    fn text_tokens() {
        let cfg = Config {
            text: "{filename} {date}".into(),
            custom: "地点：家".into(),
            ..Config::default()
        };
        let t = build_text(&cfg, Some("2021-05-03T14:15:00"), Some("exif"), "IMG_1.JPG", None);
        assert_eq!(t, "IMG_1.JPG 2021-05-03\n地点：家");
    }

    #[test]
    fn render_marks_pixels_and_respects_bounds() {
        let img = RgbaImage::from_pixel(1200, 800, Rgba([20, 20, 20, 255]));
        let cfg = Config {
            size_pct: 4.0,
            position: "br".into(),
            ..Config::default()
        };
        let out = render(img, &cfg, "2021-05-03");
        assert!(out.font_px > 30.0);
        // 水印确实画上去了，且没有溢出到左上角
        let changed = (0..out.img.height())
            .flat_map(|y| (0..out.img.width()).map(move |x| (x, y)))
            .filter(|(x, y)| out.img.get_pixel(*x, *y) != &Rgba([20, 20, 20, 255]))
            .count();
        assert!(changed > 200, "changed={changed}");
        assert_eq!(out.img.get_pixel(10, 10), &Rgba([20, 20, 20, 255]));
    }

    #[test]
    fn small_image_uses_min_size() {
        let img = RgbaImage::from_pixel(320, 240, Rgba([0, 0, 0, 255]));
        let cfg = Config {
            size_pct: 2.0,
            min_px: 14.0,
            ..Config::default()
        };
        let out = render(img, &cfg, "2021-05-03");
        assert!(out.font_px >= 13.5, "font_px={}", out.font_px);
    }

    #[test]
    fn long_text_shrinks_to_width_limit() {
        let img = RgbaImage::from_pixel(1000, 1000, Rgba([0, 0, 0, 255]));
        let cfg = Config {
            size_pct: 12.0,
            max_width_pct: 30.0,
            min_px: 6.0,
            ..Config::default()
        };
        let out = render(img, &cfg, "2021-05-03 14:15:00 这是一个很长很长的水印文字");
        assert!(out.font_px < 100.0, "font_px={}", out.font_px);
    }

    fn minimal_app1(orientation: u16) -> Vec<u8> {
        // FFE1 + len + "Exif\0\0" + II 42 + IFD0(1 条 Orientation 记录)
        let mut tiff: Vec<u8> = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let len = (2 + 6 + tiff.len()) as u16;
        let mut seg = vec![0xFF, 0xE1];
        seg.extend_from_slice(&len.to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&tiff);
        seg
    }

    #[test]
    fn orientation_reset_to_one() {
        let mut seg = minimal_app1(6);
        reset_orientation(&mut seg);
        // Orientation 值落在 TIFF 起始(10) + IFD0(8) + 2 + 8 = 28
        assert_eq!(u16::from_le_bytes([seg[28], seg[29]]), 1);
    }

    #[test]
    fn splice_keeps_exif_after_soi() {
        let dir = std::env::temp_dir();
        let src = dir.join("pvm-wm-splice-src.jpg");
        let mut fake = vec![0xFF, 0xD8];
        fake.extend_from_slice(&minimal_app1(8));
        fake.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x00]);
        std::fs::write(&src, &fake).unwrap();
        let out = splice_exif(vec![0xFF, 0xD8, 0xFF, 0xD9], &src, true);
        assert_eq!(&out[..2], &[0xFF, 0xD8]);
        assert_eq!(&out[2..4], &[0xFF, 0xE1]);
        assert!(out.windows(4).any(|w| w == b"Exif"));
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn rotation_keeps_content_visible() {
        let img = RgbaImage::from_pixel(900, 600, Rgba([10, 10, 10, 255]));
        let cfg = Config {
            rotate: 30.0,
            position: "mc".into(),
            ..Config::default()
        };
        let out = render(img, &cfg, "2021-05-03");
        assert_eq!(out.img.dimensions(), (900, 600));
    }

    #[test]
    fn free_position_uses_percentages() {
        let cfg = Config {
            position: "free".into(),
            pos_x: 10.0,
            pos_y: 20.0,
            ..Config::default()
        };
        // 1000x800 的画面，10%/20% → 左上角在 (100, 160)
        let b = block_box(&cfg, 1000, 800, 200, 50);
        assert_eq!((b.x, b.y), (100, 160), "{b:?}");
    }

    #[test]
    fn free_position_clamps_inside_frame() {
        let cfg = Config {
            position: "free".into(),
            pos_x: 100.0,
            pos_y: 100.0,
            ..Config::default()
        };
        // 贴到右下角时不能超出画面
        let b = block_box(&cfg, 1000, 800, 200, 50);
        assert!(b.x + b.w as i32 <= 1000, "{b:?}");
        assert!(b.y + b.h as i32 <= 800, "{b:?}");
        assert_eq!((b.x, b.y), (800, 750), "{b:?}");
    }

    #[test]
    fn free_position_scales_with_image_size() {
        let cfg = Config {
            position: "free".into(),
            pos_x: 50.0,
            pos_y: 50.0,
            ..Config::default()
        };
        // 同样 50%/50%，不同尺寸的照片上相对位置一致
        let a = block_box(&cfg, 1000, 800, 200, 50);
        let b = block_box(&cfg, 2000, 1600, 200, 50);
        assert_eq!(a.x, 500);
        assert_eq!(b.x, 1000);
        // 相对比例一致
        assert_eq!(a.x as f64 / 1000.0, b.x as f64 / 2000.0);
    }

    #[test]
    fn free_position_render_moves_text() {
        let mk = |x: f64| {
            let img = RgbaImage::from_pixel(800, 600, Rgba([0, 0, 0, 255]));
            let cfg = Config {
                position: "free".into(),
                pos_x: x,
                pos_y: 50.0,
                ..Config::default()
            };
            render(img, &cfg, "2021-05-03")
        };
        let l = mk(5.0);
        let r = mk(70.0);
        let lb = l.box_.expect("应有位置");
        let rb = r.box_.expect("应有位置");
        assert!(rb.x > lb.x, "右侧的水印 x 应该更大：{} vs {}", rb.x, lb.x);
    }

    #[test]
    fn grid_position_ignores_free_coords() {
        let cfg = Config {
            position: "br".into(),
            pos_x: 0.0,
            pos_y: 0.0,
            ..Config::default()
        };
        let b = block_box(&cfg, 1000, 800, 200, 50);
        // 右下角：x = 1000-200-margin(3% of 800 = 24) = 776
        assert_eq!(b.x, 776, "{b:?}");
        assert_eq!(b.y, 800 - 50 - 24, "{b:?}");
    }

    #[test]
    fn custom_font_file_accepts_valid_path() {
        // 系统里一定有字体，拿一个真实文件来验
        let Some(p) = fonts_dir().join("msyh.ttc").is_file().then(|| fonts_dir().join("msyh.ttc")) else {
            return;
        };
        let got = custom_font_file(&p.to_string_lossy());
        assert!(got.is_some(), "应该能识别这个字体文件");
        assert!(is_font_file(got.as_ref().unwrap()));
    }

    #[test]
    fn custom_font_file_rejects_bad_input() {
        assert!(custom_font_file("").is_none());
        assert!(custom_font_file("   ").is_none());
        assert!(custom_font_file("C:/definitely/not/here.ttf").is_none());
        // 扩展名不对的也要拒绝
        assert!(custom_font_file("C:/Windows/System32/cmd.exe").is_none());
    }

    #[test]
    fn is_font_file_checks_extension() {
        assert!(is_font_file(Path::new("a.ttf")));
        assert!(is_font_file(Path::new("a.TTC")));
        assert!(is_font_file(Path::new("a.otf")));
        assert!(!is_font_file(Path::new("a.exe")));
        assert!(!is_font_file(Path::new("a.jpg")));
    }

    #[test]
    fn custom_font_takes_priority_over_family() {
        let Some(p) = fonts_dir().join("msyh.ttc").is_file().then(|| fonts_dir().join("msyh.ttc")) else {
            return;
        };
        let cfg = Config {
            font: "微软雅黑".into(),
            font_path: p.to_string_lossy().to_string(),
            ..Config::default()
        };
        let f = load_font_cfg(&cfg);
        assert!(f.is_ok(), "自定义字体应当能加载：{:?}", f.err());
    }

    #[test]
    fn validate_custom_font_reports_errors() {
        let r = validate_custom_font("");
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(false));

        let r2 = validate_custom_font("C:/nope/missing.ttf");
        assert_eq!(r2.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert!(r2.get("error").and_then(|v| v.as_str()).is_some());
    }
}
