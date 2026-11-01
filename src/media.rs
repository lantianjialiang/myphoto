//! 元数据提取（EXIF / 视频容器）与缩略图生成 —— 全部只读，不触碰原件。

use crate::dateinfer::{candidate, Candidate, Precision};
use crate::util;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub struct Extracted {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub camera: Option<String>,
    pub gps: Option<(f64, f64)>,
    pub candidates: Vec<Candidate>,
    pub details: serde_json::Value,
    pub error: Option<String>,
}

impl Extracted {
    fn empty() -> Extracted {
        Extracted {
            width: None,
            height: None,
            duration_ms: None,
            camera: None,
            gps: None,
            candidates: Vec::new(),
            details: serde_json::json!({}),
            error: None,
        }
    }
}

fn parse_exif_time(raw: &str) -> Option<NaiveDateTime> {
    let raw = raw.trim();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(v) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(v);
        }
    }
    for fmt in ["%Y-%m-%d", "%Y:%m:%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(raw, fmt) {
            return Some(NaiveDateTime::new(d, NaiveTime::from_hms_opt(0, 0, 0)?));
        }
    }
    None
}

fn parse_offset(raw: &str) -> Option<i32> {
    let t = raw.trim();
    let (sign, rest) = match t.chars().next()? {
        '+' => (1, &t[1..]),
        '-' => (-1, &t[1..]),
        _ => return None,
    };
    let mut parts = rest.split(':');
    let h: i32 = parts.next()?.parse().ok()?;
    let m: i32 = parts.next().unwrap_or("0").parse().ok()?;
    Some(sign * (h * 60 + m))
}

fn rational_deg(field: &exif::Field, negative: bool) -> Option<f64> {
    let vals: Vec<f64> = match &field.value {
        exif::Value::Rational(v) => v.iter().map(|r| r.to_f64()).collect(),
        _ => return None,
    };
    if vals.len() < 3 {
        return None;
    }
    let deg = vals[0] + vals[1] / 60.0 + vals[2] / 3600.0;
    Some(if negative { -deg } else { deg })
}

fn read_exif(path: &Path) -> (Extracted, Option<u16>) {
    match File::open(path) {
        Ok(f) => read_exif_from(BufReader::new(f)),
        Err(e) => {
            let mut out = Extracted::empty();
            out.error = Some(format!("无法打开文件：{e}"));
            (out, None)
        }
    }
}

/// 解析 HEIF 容器里取出的 EXIF（已经是 TIFF 结构）。
pub fn read_exif_tiff(bytes: &[u8]) -> (Extracted, Option<u16>) {
    let out = Extracted::empty();
    match exif::Reader::new().read_raw(bytes.to_vec()) {
        Ok(v) => parse_exif(&v, out),
        Err(exif::Error::NotFound(_)) => (out, None),
        Err(e) => {
            let mut out = out;
            out.error = Some(format!("EXIF 解析失败：{e}"));
            (out, None)
        }
    }
}

fn read_exif_from<R: Read + Seek + std::io::BufRead>(mut reader: R) -> (Extracted, Option<u16>) {
    let mut out = Extracted::empty();
    let exif = match exif::Reader::new().read_from_container(&mut reader) {
        Ok(v) => v,
        Err(exif::Error::NotFound(_)) => return (out, None),
        Err(e) => {
            // 损坏或非标准 EXIF：记录原因，继续用其他线索（文件名/目录/文件时间）
            out.error = Some(format!("EXIF 解析失败：{e}"));
            return (out, None);
        }
    };
    parse_exif(&exif, out)
}

fn parse_exif(exif: &exif::Exif, mut out: Extracted) -> (Extracted, Option<u16>) {
    let mut orientation = None;
    let mut raw = serde_json::Map::new();
    let mut offset_min: Option<i32> = None;
    let mut taken: Option<(NaiveDateTime, String)> = None;

    if let Some(f) = exif.get_field(exif::Tag::OffsetTimeOriginal, exif::In::PRIMARY) {
        offset_min = parse_offset(&f.display_value().to_string());
    }

    for tag in [
        exif::Tag::DateTimeOriginal,
        exif::Tag::DateTimeDigitized,
        exif::Tag::DateTime,
    ] {
        if let Some(f) = exif.get_field(tag, exif::In::PRIMARY) {
            let text = f.display_value().to_string();
            raw.insert(format!("{tag:?}"), serde_json::json!(text));
            if taken.is_none() {
                if let Some(dt) = parse_exif_time(&text) {
                    taken = Some((dt, format!("EXIF {tag:?}")));
                }
            }
        }
    }
    if let Some(f) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        orientation = f.value.get_uint(0).map(|v| v as u16);
    }
    let make = exif
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string().trim_matches('"').to_string());
    let model = exif
        .get_field(exif::Tag::Model, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string().trim_matches('"').to_string());
    if let Some(m) = &make {
        raw.insert("Make".into(), serde_json::json!(m));
    }
    if let Some(m) = &model {
        raw.insert("Model".into(), serde_json::json!(m));
    }
    let camera = match (make, model) {
        (Some(a), Some(b)) => Some(if b.to_lowercase().contains(&a.to_lowercase()) {
            b
        } else {
            format!("{a} {b}")
        }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        _ => None,
    };
    for tag in [exif::Tag::PixelXDimension, exif::Tag::PixelYDimension] {
        if let Some(f) = exif.get_field(tag, exif::In::PRIMARY) {
            let v = f.value.get_uint(0).map(|v| v as i64);
            if tag == exif::Tag::PixelXDimension {
                out.width = v.or(out.width);
            } else {
                out.height = v.or(out.height);
            }
        }
    }
    // GPS
    let lat = exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY);
    let lon = exif.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY);
    if let (Some(la), Some(lo)) = (lat, lon) {
        let lat_ref = exif
            .get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        let lon_ref = exif
            .get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        if let (Some(la), Some(lo)) = (
            rational_deg(la, lat_ref.starts_with('S')),
            rational_deg(lo, lon_ref.starts_with('W')),
        ) {
            out.gps = Some((la, lo));
            raw.insert("GPS".into(), serde_json::json!(format!("{la:.6},{lo:.6}")));
        }
    }
    if let Some((dt, detail)) = taken {
        let precision = if dt.time() == NaiveTime::from_hms_opt(0, 0, 0).unwrap() {
            Precision::Day
        } else {
            Precision::Second
        };
        out.candidates.push(candidate(
            dt,
            precision,
            "exif",
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            offset_min,
            &detail,
        ));
    }
    out.camera = camera;
    out.details = serde_json::json!({ "exif": raw, "has_exif": true });
    (out, orientation)
}

// ---------- MP4 / MOV 容器解析 ----------

pub struct VideoInfo {
    pub creation: Option<NaiveDateTime>,
    pub creation_is_utc: bool,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub codec: Option<String>,
    pub brand: String,
}

fn read_u32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn fourcc(b: &[u8], o: usize) -> String {
    b.get(o..o + 4)
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_default()
}

struct BoxRef {
    kind: String,
    payload: Vec<u8>,
}

/// 遍历一段内存中的 box 序列。
fn children(data: &[u8]) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o + 8 <= data.len() {
        let mut size = read_u32(data, o).unwrap_or(0) as usize;
        let kind = fourcc(data, o + 4);
        let mut header = 8usize;
        if size == 1 {
            match read_u64(data, o + 8) {
                Some(v) => {
                    size = v as usize;
                    header = 16;
                }
                None => break,
            }
        } else if size == 0 {
            size = data.len() - o;
        }
        if size < header || o + size > data.len() {
            // 损坏或截断：停止解析，不影响其他信息
            break;
        }
        out.push(BoxRef {
            kind,
            payload: data[o + header..o + size].to_vec(),
        });
        o += size;
    }
    out
}

fn find<'a>(list: &'a [BoxRef], kind: &str) -> Option<&'a BoxRef> {
    list.iter().find(|b| b.kind == kind)
}

pub fn probe_video(path: &Path) -> Result<VideoInfo, String> {
    let mut file = File::open(path).map_err(|e| format!("无法打开文件：{e}"))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut brand = String::new();
    let mut is_qt = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mov"))
        .unwrap_or(false);
    let mut moov: Option<Vec<u8>> = None;
    let mut pos = 0u64;
    while pos + 8 <= total {
        file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut header = [0u8; 16];
        let n = file.read(&mut header).map_err(|e| e.to_string())?;
        if n < 8 {
            break;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind = String::from_utf8_lossy(&header[4..8]).to_string();
        let mut hlen = 8u64;
        if size == 1 {
            if n < 16 {
                break;
            }
            size = u64::from_be_bytes([
                header[8], header[9], header[10], header[11], header[12], header[13], header[14],
                header[15],
            ]);
            hlen = 16;
        } else if size == 0 {
            size = total - pos;
        }
        if size < hlen {
            break;
        }
        if kind == "ftyp" {
            let _ = file.seek(SeekFrom::Start(pos + hlen));
            let mut buf = vec![0u8; (size - hlen).min(1024) as usize];
            let _ = file.read(&mut buf);
            brand = fourcc(&buf, 0);
            let compat = String::from_utf8_lossy(&buf).to_string();
            if compat.contains("qt  ") {
                is_qt = true;
            }
        } else if kind == "moov" && size <= 64 * 1024 * 1024 {
            file.seek(SeekFrom::Start(pos + hlen)).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; (size - hlen) as usize];
            file.read_exact(&mut buf).map_err(|e| e.to_string())?;
            moov = Some(buf);
            if brand.is_empty() {
                break;
            }
        }
        pos += size;
    }
    let moov = moov.ok_or_else(|| "未找到 moov 元数据（文件可能损坏或不是标准 MP4/MOV）".to_string())?;
    let top = children(&moov);
    let mut info = VideoInfo {
        creation: None,
        creation_is_utc: !is_qt,
        duration_ms: None,
        width: None,
        height: None,
        codec: None,
        brand: if brand.is_empty() {
            if is_qt { "qt  ".into() } else { "?".into() }
        } else {
            brand
        },
    };
    if let Some(mvhd) = find(&top, "mvhd") {
        let p = &mvhd.payload;
        let version = p.first().copied().unwrap_or(0);
        let (created, timescale, duration) = if version == 1 {
            (
                read_u64(p, 4),
                read_u32(p, 20).map(|v| v as u64),
                read_u64(p, 24),
            )
        } else {
            (
                read_u32(p, 4).map(|v| v as u64),
                read_u32(p, 12).map(|v| v as u64),
                read_u32(p, 16).map(|v| v as u64),
            )
        };
        if let (Some(ts), Some(d)) = (timescale, duration) {
            if ts > 0 && d != u32::MAX as u64 && d != u64::MAX {
                info.duration_ms = Some((d as f64 * 1000.0 / ts as f64).round() as i64);
            }
        }
        if let Some(c) = created {
            if c > 2_082_844_800 {
                let unix = c as i64 - 2_082_844_800;
                if (315_532_800..=4_102_444_800).contains(&unix) {
                    let naive_utc = chrono::DateTime::from_timestamp(unix, 0)
                        .map(|dt| dt.naive_utc());
                    if let Some(nu) = naive_utc {
                        info.creation = Some(if info.creation_is_utc {
                            chrono::Local.from_utc_datetime(&nu).naive_local()
                        } else {
                            nu
                        });
                    }
                }
            }
        }
    }
    for trak in top.iter().filter(|b| b.kind == "trak") {
        let tk = children(&trak.payload);
        if let Some(tkhd) = find(&tk, "tkhd") {
            let p = &tkhd.payload;
            let version = p.first().copied().unwrap_or(0);
            let base = if version == 1 { 96 } else { 84 };
            if let (Some(w), Some(h)) = (read_u32(p, base), read_u32(p, base + 4)) {
                let w = (w >> 16) as i64;
                let h = (h >> 16) as i64;
                if w > 0 && h > 0 && (info.width.is_none() || w * h > info.width.unwrap() * info.height.unwrap_or(0)) {
                    info.width = Some(w);
                    info.height = Some(h);
                }
            }
        }
        if let Some(mdia) = find(&tk, "mdia") {
            let md = children(&mdia.payload);
            if let Some(minf) = find(&md, "minf") {
                let mi = children(&minf.payload);
                if let Some(stbl) = find(&mi, "stbl") {
                    let sb = children(&stbl.payload);
                    if let Some(stsd) = find(&sb, "stsd") {
                        if let Some(codec) = read_u32(&stsd.payload, 12).map(|_| fourcc(&stsd.payload, 12)).filter(|s| !s.is_empty()) {
                            if info.codec.is_none() {
                                info.codec = Some(codec);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(info)
}

pub fn extract(path: &Path, ext: &str, kind: &str, dirs: &[String], mtime_ms: i64) -> Extracted {
    let mut out = match kind {
        "image" => {
            let heif = matches!(ext, "heic" | "heif");
            let (mut e, mut orientation) = if heif {
                // HEIC 的 EXIF 在 HEIF 容器的 Exif item 里，先让 libheif 取出来再按 TIFF 解析。
                match crate::heif::exif_tiff(path) {
                    Some(tiff) => read_exif_tiff(&tiff),
                    None => (Extracted::empty(), None),
                }
            } else {
                read_exif(path)
            };
            if heif {
                // 旋转由 HEIF 的 irot/imir 属性表达，libheif 解码时已应用，
                // 因此有解码器时宽高直接用它给的（已含旋转），不再叠加 EXIF Orientation。
                match crate::heif::probe(path) {
                    Ok((w, h)) => {
                        e.width = Some(w as i64);
                        e.height = Some(h as i64);
                        orientation = None;
                    }
                    Err(_) => {
                        // 没有解码器时退回解析 ipco/ispe，至少把宽高记下来。
                        if let Some((w, h)) = std::fs::read(path)
                            .ok()
                            .and_then(|b| crate::heif::fallback_dimensions(&b))
                        {
                            e.width = Some(w as i64);
                            e.height = Some(h as i64);
                        }
                    }
                }
            }
            if e.width.is_none() || e.height.is_none() {
                if let Ok((w, h)) = image::image_dimensions(path) {
                    e.width = Some(w as i64);
                    e.height = Some(h as i64);
                }
            }
            if let Some(o) = orientation {
                if matches!(o, 5..=8) {
                    std::mem::swap(&mut e.width, &mut e.height);
                }
            }
            if heif && !crate::heif::available() {
                let status = crate::heif::status();
                e.error = Some(format!(
                    "HEIC/HEIF 未启用：{}；仍可索引、去重与归档，暂无缩略图",
                    status.detail
                ));
            } else if !util::decodable_image(ext) && !heif {
                e.error = Some(format!(
                    "{} 格式当前版本未内置解码器，可索引但无法生成缩略图/预览",
                    ext.to_uppercase()
                ));
            }
            e
        }
        _ => {
            let mut e = Extracted::empty();
            match probe_video(path) {
                Ok(v) => {
                    if let Some(c) = v.creation {
                        let precision = Precision::Second;
                        e.candidates.push(candidate(
                            c,
                            precision,
                            "video",
                            c.format("%Y-%m-%d %H:%M:%S").to_string(),
                            if v.creation_is_utc { Some(local_offset_min(c)) } else { None },
                            if v.creation_is_utc {
                                "视频容器创建时间（UTC 已转本地）"
                            } else {
                                "QuickTime 容器创建时间（按本地时间解释）"
                            },
                        ));
                    }
                    e.width = v.width;
                    e.height = v.height;
                    e.duration_ms = v.duration_ms;
                    if let Some(codec) = &v.codec {
                        if matches!(codec.as_str(), "hvc1" | "hev1") {
                            e.error = Some("HEVC 编码：Windows 未安装 HEVC 扩展时无法播放，将显示封面占位".into());
                        }
                    }
                    e.details = serde_json::json!({
                        "video": {
                            "brand": v.brand,
                            "codec": v.codec,
                            "duration_ms": v.duration_ms,
                            "creation_time_is_utc": v.creation_is_utc,
                        }
                    });
                }
                Err(err) => {
                    e.error = Some(err);
                    e.details = serde_json::json!({ "video": { "error": "无法解析容器" } });
                }
            }
            e
        }
    };
    if out.candidates.is_empty() {
        if let Some((dt, precision, raw)) = crate::dateinfer::parse_text(&util::file_name_of(path)) {
            out.candidates.push(candidate(
                dt,
                precision,
                "filename",
                raw,
                None,
                "来自文件名",
            ));
        }
    }
    if out.candidates.is_empty() {
        if let Some(c) = crate::dateinfer::folder_candidate(dirs) {
            out.candidates.push(c);
        }
    }
    if let Some(c) = crate::dateinfer::filesystem_candidate(mtime_ms) {
        out.candidates.push(c);
    }
    out
}

fn local_offset_min(dt: NaiveDateTime) -> i32 {
    use chrono::Offset;
    chrono::Local
        .from_local_datetime(&dt)
        .single()
        .map(|d| d.offset().fix().local_minus_utc() / 60)
        .unwrap_or(0)
}

// ---------- 全尺寸解码（含方向校正） ----------

/// 解码一张照片为已按 EXIF 方向校正的图像。HEIC/HEIF 走随包解码器，
/// 解码器缺失时返回明确错误，由调用方按失败项处理（不猜内容）。
pub fn load_oriented(src: &Path) -> Result<image::DynamicImage, String> {
    let ext = util::ext_of(src);
    if matches!(ext.as_str(), "heic" | "heif") {
        // 直接拿完整 RGB，避免「解码 → JPEG → 再解码」的二次压缩与内存放大。
        let rgb = crate::heif::decode_rgb(src)?;
        return Ok(image::DynamicImage::ImageRgb8(rgb));
    }
    let reader = image::ImageReader::open(src)
        .map_err(|e| format!("无法打开：{e}"))?
        .with_guessed_format()
        .map_err(|e| format!("无法识别格式：{e}"))?;
    let img = reader.decode().map_err(|e| format!("解码失败：{e}"))?;
    Ok(apply_orientation(img, image_orientation(src)))
}

// ---------- 缩略图 ----------

pub fn thumbnail(src: &Path, max_side: u32) -> Result<Vec<u8>, String> {
    let ext = util::ext_of(src);
    if matches!(ext.as_str(), "heic" | "heif") {
        return crate::heif::thumbnail_jpeg(src, max_side);
    }
    let reader = image::ImageReader::open(src)
        .map_err(|e| format!("无法打开：{e}"))?
        .with_guessed_format()
        .map_err(|e| format!("无法识别格式：{e}"))?;
    let img = reader.decode().map_err(|e| format!("解码失败：{e}"))?;
    let orientation = image_orientation(src);
    let img = apply_orientation(img, orientation);
    let thumb = img.thumbnail(max_side, max_side);
    let rgb = thumb.to_rgb8();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82);
    enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| format!("编码失败：{e}"))?;
    Ok(buf)
}

/// 读取图片像素尺寸（已按 EXIF 方向校正，即用户看到的宽高）。
/// 读不出来返回 None，调用方据此决定是否更新索引。
pub fn probe_dimensions(path: &str) -> Option<(u32, u32)> {
    let p = Path::new(path);
    let ext = util::ext_of(p);
    if matches!(ext.as_str(), "heic" | "heif") {
        let rgb = crate::heif::decode_rgb(p).ok()?;
        return Some((rgb.width(), rgb.height()));
    }
    let reader = image::ImageReader::open(p).ok()?.with_guessed_format().ok()?;
    let img = reader.decode().ok()?;
    let img = apply_orientation(img, image_orientation(p));
    Some((img.width(), img.height()))
}

pub fn image_orientation(src: &Path) -> u16 {
    let Ok(file) = File::open(src) else { return 1 };
    let mut reader = BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1) as u16
}

fn apply_orientation(img: image::DynamicImage, orientation: u16) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// 闰年判断。日期推断模块扩展时会用到，先留作公共工具。
#[allow(dead_code)]
pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// 某年某月的天数。日期推断模块扩展时会用到，先留作公共工具。
#[allow(dead_code)]
pub fn days_in_month(y: i32, m: u32) -> u32 {
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.succ_opt())
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造最小可解析的 MP4（仅含 ftyp + moov/mvhd + trak/tkhd）。
    fn fake_mp4(created_unix: u64, is_qt: bool) -> Vec<u8> {
        fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
            v.extend_from_slice(kind);
            v.extend_from_slice(payload);
            v
        }
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(if is_qt { b"qt  " } else { b"isom" });
        ftyp.extend_from_slice(&[0, 0, 2, 0]);
        ftyp.extend_from_slice(if is_qt { b"qt  " } else { b"isom" });
        ftyp.extend_from_slice(b"mp41");
        let ftyp = boxed(b"ftyp", &ftyp);

        let mut mvhd = vec![0u8; 100];
        mvhd[0] = 0; // version 0
        mvhd[4..8].copy_from_slice(&((created_unix + 2_082_844_800) as u32).to_be_bytes());
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd[16..20].copy_from_slice(&5500u32.to_be_bytes()); // duration
        let mvhd = boxed(b"mvhd", &mvhd);

        let mut tkhd = vec![0u8; 92];
        tkhd[0] = 0;
        let w = (1920u32) << 16;
        let h = (1080u32) << 16;
        tkhd[84..88].copy_from_slice(&w.to_be_bytes());
        tkhd[88..92].copy_from_slice(&h.to_be_bytes());
        let tkhd = boxed(b"tkhd", &tkhd);
        let trak = boxed(b"trak", &tkhd);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd);
        moov.extend_from_slice(&trak);
        let moov = boxed(b"moov", &moov);

        let mut out = Vec::new();
        out.extend_from_slice(&ftyp);
        out.extend_from_slice(&moov);
        // 尾部填充模拟 mdat
        out.extend_from_slice(&[0, 0, 0, 8]);
        out.extend_from_slice(b"mdat");
        out
    }

    #[test]
    fn probes_mp4_container() {
        let dir = std::env::temp_dir().join(format!("pvm-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, fake_mp4(1_683_000_000, false)).unwrap();
        let info = probe_video(&path).unwrap();
        assert_eq!(info.duration_ms, Some(5500));
        assert_eq!(info.width, Some(1920));
        assert_eq!(info.height, Some(1080));
        assert!(info.creation.is_some());
        assert!(info.creation_is_utc);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quicktime_time_is_treated_as_local() {
        let dir = std::env::temp_dir().join(format!("pvm-video-qt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, fake_mp4(1_683_000_000, true)).unwrap();
        let info = probe_video(&path).unwrap();
        assert!(!info.creation_is_utc);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_container_reports_error_without_panic() {
        let dir = std::env::temp_dir().join(format!("pvm-video-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.mp4");
        std::fs::write(&path, b"not a real mp4 at all............").unwrap();
        assert!(probe_video(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
