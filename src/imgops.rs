//! 图像像素级操作：旋转翻转、裁剪、调色、局部打码。
//!
//! 这一层是**纯函数**：输入一张图，输出一张图，不碰任何文件。
//! 文件层面的备份、原子替换、索引更新由 `edit.rs` 负责。
//!
//! 裁剪与打码的位置都用**百分比**表示（0-100），这样同一个参数
//! 应用到不同尺寸的照片上时，相对位置保持一致。

use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// 旋转 / 翻转。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// 顺时针旋转 90°
    Rot90,
    /// 旋转 180°
    Rot180,
    /// 逆时针旋转 90°
    Rot270,
    /// 左右镜像
    FlipH,
    /// 上下镜像
    FlipV,
}

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rot90" | "cw90" | "right90" => Some(Op::Rot90),
            "rot180" | "180" => Some(Op::Rot180),
            "rot270" | "ccw90" | "left90" => Some(Op::Rot270),
            "fliph" | "flip_h" | "mirror_h" => Some(Op::FlipH),
            "flipv" | "flip_v" | "mirror_v" => Some(Op::FlipV),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Op::Rot90 => "顺时针 90°",
            Op::Rot180 => "180°",
            Op::Rot270 => "逆时针 90°",
            Op::FlipH => "左右翻转",
            Op::FlipV => "上下翻转",
        }
    }

    /// 对已按 EXIF 方向校正的像素做变换。
    pub fn apply(&self, img: RgbaImage) -> RgbaImage {
        match self {
            Op::Rot90 => image::imageops::rotate90(&img),
            Op::Rot180 => image::imageops::rotate180(&img),
            Op::Rot270 => image::imageops::rotate270(&img),
            Op::FlipH => image::imageops::flip_horizontal(&img),
            Op::FlipV => image::imageops::flip_vertical(&img),
        }
    }
}

// ---------------- 矩形与对齐 ----------------

/// 百分比矩形：左上角 + 宽高，范围 0-100。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for Rect {
    fn default() -> Self {
        Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }
    }
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// 收敛到合法范围：宽高至少 1%，且不越出边界。
    pub fn clean(mut self) -> Rect {
        self.w = self.w.clamp(1.0, 100.0);
        self.h = self.h.clamp(1.0, 100.0);
        self.x = self.x.clamp(0.0, 100.0 - self.w);
        self.y = self.y.clamp(0.0, 100.0 - self.h);
        self
    }

    /// 换算成像素矩形 (x, y, w, h)，保证至少 1 像素且不越界。
    pub fn pixels(self, img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
        let r = self.clean();
        let x = (r.x / 100.0 * img_w as f32).round().max(0.0) as u32;
        let y = (r.y / 100.0 * img_h as f32).round().max(0.0) as u32;
        let w = ((r.w / 100.0 * img_w as f32).round() as u32).max(1).min(img_w.saturating_sub(x));
        let h = ((r.h / 100.0 * img_h as f32).round() as u32).max(1).min(img_h.saturating_sub(y));
        (x, y, w.max(1), h.max(1))
    }
}

/// 水平对齐方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

impl HAlign {
    pub fn parse(s: &str) -> HAlign {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => HAlign::Left,
            "right" | "r" => HAlign::Right,
            _ => HAlign::Center,
        }
    }
}

/// 垂直对齐方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

impl VAlign {
    pub fn parse(s: &str) -> VAlign {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" | "t" => VAlign::Top,
            "bottom" | "b" => VAlign::Bottom,
            _ => VAlign::Center,
        }
    }
}

// ---------------- 裁剪 ----------------

/// 裁剪方式，二选一：
/// * `rect` —— 自由区域（百分比），适合"统一去掉底部一条"这类固定位置的裁法；
/// * `aspect` —— 固定比例 + 对齐，批量处理时每张图输出比例一致。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub rect: Option<Rect>,
    /// 目标宽高比（宽 / 高），例如 1.0 表示 1:1
    pub aspect: Option<f64>,
    pub h: HAlign,
    pub v: VAlign,
}

impl Default for Crop {
    fn default() -> Self {
        Crop { rect: None, aspect: None, h: HAlign::Center, v: VAlign::Center }
    }
}

impl Crop {
    pub fn label(&self) -> &'static str {
        if self.aspect.is_some() {
            "按比例裁剪"
        } else {
            "按区域裁剪"
        }
    }

    pub fn apply(&self, img: RgbaImage) -> RgbaImage {
        if let Some(a) = self.aspect {
            if a > 0.0 {
                return crop_by_aspect(&img, a, self.h, self.v);
            }
        }
        match self.rect {
            Some(r) => crop_by_rect(&img, r),
            None => img,
        }
    }
}

/// 在图里挖出一块矩形（像素坐标），返回新的 RgbaImage。
fn crop_by_rect(img: &RgbaImage, rect: Rect) -> RgbaImage {
    let (x, y, w, h) = rect.pixels(img.width(), img.height());
    copy_region(img, x, y, w, h)
}

/// 把区域里的像素复制成一张新图。
pub fn copy_region(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
    let mut out = RgbaImage::new(w.max(1), h.max(1));
    for j in 0..out.height() {
        let sy = y + j;
        if sy >= img.height() {
            break;
        }
        for i in 0..out.width() {
            let sx = x + i;
            if sx >= img.width() {
                break;
            }
            out.put_pixel(i, j, *img.get_pixel(sx, sy));
        }
    }
    out
}

/// 按目标比例裁剪：在图内取能容纳该比例的最大矩形，再按对齐方式摆放。
fn crop_by_aspect(img: &RgbaImage, aspect: f64, h: HAlign, v: VAlign) -> RgbaImage {
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    let (cw, ch) = if iw / ih > aspect {
        (ih * aspect, ih)
    } else {
        (iw, iw / aspect)
    };
    let dx = match h {
        HAlign::Left => 0.0,
        HAlign::Right => iw - cw,
        HAlign::Center => (iw - cw) / 2.0,
    };
    let dy = match v {
        VAlign::Top => 0.0,
        VAlign::Bottom => ih - ch,
        VAlign::Center => (ih - ch) / 2.0,
    };
    crop_by_rect(
        img,
        Rect::new(
            (dx / iw * 100.0) as f32,
            (dy / ih * 100.0) as f32,
            (cw / iw * 100.0) as f32,
            (ch / ih * 100.0) as f32,
        ),
    )
}

// ---------------- 调色 ----------------

/// 调色预设：内部会映射成一组具体的参数。
pub const TONE_PRESETS: &[(&str, &str)] = &[
    ("custom", "自定义"),
    ("mono", "黑白"),
    ("sepia", "棕调"),
    ("cold", "冷调"),
    ("warm", "暖调"),
    ("fade", "褪色"),
    ("vivid", "鲜艳"),
];

/// 调色参数。取值范围见 `clean`。
#[derive(Clone, Debug, PartialEq)]
pub struct Tone {
    /// 亮度，-100 ~ 100
    pub brightness: f32,
    /// 对比度，-100 ~ 100
    pub contrast: f32,
    /// 饱和度，-100 ~ 100
    pub saturation: f32,
    /// 色温，-100（冷）~ 100（暖）
    pub warmth: f32,
    /// 预设名，见 `TONE_PRESETS`
    pub preset: String,
}

impl Default for Tone {
    fn default() -> Self {
        Tone { brightness: 0.0, contrast: 0.0, saturation: 0.0, warmth: 0.0, preset: "custom".into() }
    }
}

impl Tone {
    pub fn clean(&self) -> Tone {
        Tone {
            brightness: self.brightness.clamp(-100.0, 100.0),
            contrast: self.contrast.clamp(-100.0, 100.0),
            saturation: self.saturation.clamp(-100.0, 100.0),
            warmth: self.warmth.clamp(-100.0, 100.0),
            preset: self.preset.clone(),
        }
    }

    /// 是否等于什么都没做。
    pub fn is_noop(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 0.0
            && self.saturation == 0.0
            && self.warmth == 0.0
            && matches!(self.preset.as_str(), "" | "custom" | "none")
    }

    pub fn label(&self) -> String {
        if let Some((_, name)) = TONE_PRESETS.iter().find(|(k, _)| *k == self.preset) {
            if self.preset != "custom" {
                return format!("{name}调色");
            }
        }
        "调色".to_string()
    }

    /// 预设映射成具体参数值：亮度、对比度、饱和度、色温、是否走棕调曲线。
    fn resolved(&self) -> (f32, f32, f32, f32, bool) {
        match self.preset.as_str() {
            "mono" => (0.0, 12.0, -100.0, 0.0, false),
            "sepia" => (4.0, 6.0, -28.0, 26.0, true),
            "cold" => (0.0, 8.0, 8.0, -32.0, false),
            "warm" => (2.0, 6.0, 10.0, 32.0, false),
            "fade" => (10.0, -22.0, -26.0, 6.0, false),
            "vivid" => (0.0, 16.0, 38.0, 4.0, false),
            _ => (self.brightness, self.contrast, self.saturation, self.warmth, false),
        }
    }

    pub fn apply(&self, img: &RgbaImage) -> RgbaImage {
        let t = self.clean();
        if t.is_noop() {
            return img.clone();
        }
        let (b, c, s, w, sepia) = t.resolved();
        let b_off = b / 100.0 * 100.0;
        let c_factor = ((100.0 + c) / 100.0).clamp(0.0, 2.0);
        let s_factor = (1.0 + s / 100.0).clamp(0.0, 2.0);
        let w_shift = w / 100.0 * 40.0;

        let mut out = img.clone();
        for p in out.pixels_mut() {
            let (mut r, mut g, mut b_) = (p[0] as f32, p[1] as f32, p[2] as f32);
            // 亮度
            r += b_off;
            g += b_off;
            b_ += b_off;
            // 对比度（围绕中灰 128）
            r = (r - 128.0) * c_factor + 128.0;
            g = (g - 128.0) * c_factor + 128.0;
            b_ = (b_ - 128.0) * c_factor + 128.0;
            // 饱和度（按亮度混合）
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b_;
            r = luma + (r - luma) * s_factor;
            g = luma + (g - luma) * s_factor;
            b_ = luma + (b_ - luma) * s_factor;
            // 色温：暖则加红减蓝
            r += w_shift;
            b_ -= w_shift;
            if sepia {
                let (sr, sg, sb) = (r, g, b_);
                r = 0.393 * sr + 0.769 * sg + 0.189 * sb;
                g = 0.349 * sr + 0.686 * sg + 0.168 * sb;
                b_ = 0.272 * sr + 0.534 * sg + 0.131 * sb;
            }
            p[0] = r.clamp(0.0, 255.0) as u8;
            p[1] = g.clamp(0.0, 255.0) as u8;
            p[2] = b_.clamp(0.0, 255.0) as u8;
        }
        out
    }
}

// ---------------- 局部打码 ----------------

/// 打码方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskMode {
    /// 马赛克（色块化）
    Mosaic,
    /// 高斯模糊
    Blur,
}

impl MaskMode {
    pub fn parse(s: &str) -> Option<MaskMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mosaic" | "pixel" | "block" => Some(MaskMode::Mosaic),
            "blur" | "gauss" => Some(MaskMode::Blur),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            MaskMode::Mosaic => "马赛克",
            MaskMode::Blur => "模糊",
        }
    }
}

/// 局部打码：对指定区域做马赛克或模糊。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mask {
    pub rect: Rect,
    pub mode: MaskMode,
    /// 马赛克色块边长（像素）或模糊半径（像素）
    pub strength: f32,
}

impl Default for Mask {
    fn default() -> Self {
        Mask { rect: Rect::new(60.0, 60.0, 25.0, 20.0), mode: MaskMode::Mosaic, strength: 16.0 }
    }
}

impl Mask {
    pub fn label(&self) -> String {
        format!("{}打码", self.mode.label())
    }

    pub fn apply(&self, img: &RgbaImage) -> RgbaImage {
        let (x, y, w, h) = self.rect.clean().pixels(img.width(), img.height());
        match self.mode {
            MaskMode::Mosaic => mosaic_region(img, x, y, w, h, self.strength),
            MaskMode::Blur => blur_region(img, x, y, w, h, self.strength),
        }
    }
}

/// 马赛克：把区域内每个色块的像素替换成该块的平均色。
fn mosaic_region(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32, block: f32) -> RgbaImage {
    let block = block.clamp(2.0, 96.0) as u32;
    let mut out = img.clone();
    let mut by = 0u32;
    while by < h {
        let bh = block.min(h - by);
        let mut bx = 0u32;
        while bx < w {
            let bw = block.min(w - bx);
            let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    let p = img.get_pixel(x + xx, y + yy);
                    sr += p[0] as u32;
                    sg += p[1] as u32;
                    sb += p[2] as u32;
                    n += 1;
                }
            }
            let c = image::Rgba([
                (sr / n.max(1)) as u8,
                (sg / n.max(1)) as u8,
                (sb / n.max(1)) as u8,
                255,
            ]);
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    out.put_pixel(x + xx, y + yy, c);
                }
            }
            bx += block;
        }
        by += block;
    }
    out
}

/// 高斯模糊：只作用于区域内，区域外保持原样。
fn blur_region(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32, sigma: f32) -> RgbaImage {
    let sigma = sigma.clamp(0.5, 60.0);
    let region = copy_region(img, x, y, w, h);
    let blurred = image::imageops::fast_blur(&region, sigma);
    let mut out = img.clone();
    for j in 0..blurred.height() {
        let dy = y + j;
        if dy >= img.height() {
            break;
        }
        for i in 0..blurred.width() {
            let dx = x + i;
            if dx >= img.width() {
                break;
            }
            out.put_pixel(dx, dy, *blurred.get_pixel(i, j));
        }
    }
    out
}

// ---------------- 统一动作 ----------------

/// 一次编辑动作。`edit.rs` 按这个执行，文件层面的事它管。
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Rotate(Op),
    Crop(Crop),
    Tone(Tone),
    Mask(Mask),
    /// 原样返回，不做任何处理。
    ///
    /// 裁剪与打码要框选区域，而框是按**原图**百分比记录的；
    /// 如果预览画的是处理后的结果，框选坐标就会和实际处理错位。
    /// 前端用这个动作单独取一张"原图参照"来框选。
    Raw,
}
impl Action {
    /// 从请求参数解析。`op` 是旧接口的旋转/翻转名，`action` + `params` 是新接口。
    pub fn from_params(op: &str, action: &str, params: &serde_json::Value) -> Result<Action, String> {
        let op = op.trim().to_ascii_lowercase();
        if !op.is_empty() {
            return Op::parse(&op)
                .map(Action::Rotate)
                .ok_or_else(|| format!("未知的操作：{op}"));
        }
        let action = action.trim().to_ascii_lowercase();
        let num = |key: &str, default: f32| -> f32 {
            params.get(key).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default)
        };
        match action.as_str() {
            "" | "rotate" | "flip" => Err("请指定操作类型".into()),
            "raw" | "none" => Ok(Action::Raw),
            "crop" => {
                let rect = params.get("rect").and_then(|v| serde_json::from_value::<Rect>(v.clone()).ok());
                let aspect = params
                    .get("aspect")
                    .and_then(|v| v.as_f64())
                    .filter(|a| *a > 0.0);
                if rect.is_none() && aspect.is_none() {
                    return Err("裁剪需要指定区域或比例".into());
                }
                let h = HAlign::parse(params.get("h").and_then(|v| v.as_str()).unwrap_or("center"));
                let v = VAlign::parse(params.get("v").and_then(|v| v.as_str()).unwrap_or("center"));
                Ok(Action::Crop(Crop { rect, aspect, h, v }))
            }
            "tone" | "color" | "adjust" => {
                let preset = params
                    .get("preset")
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom")
                    .to_ascii_lowercase();
                if !TONE_PRESETS.iter().any(|(k, _)| *k == preset) {
                    return Err(format!("未知的调色预设：{preset}"));
                }
                Ok(Action::Tone(
                    Tone {
                        brightness: num("brightness", 0.0),
                        contrast: num("contrast", 0.0),
                        saturation: num("saturation", 0.0),
                        warmth: num("warmth", 0.0),
                        preset,
                    }
                    .clean(),
                ))
            }
            "mask" | "redact" => {
                let Some(rect) = params.get("rect").and_then(|v| serde_json::from_value::<Rect>(v.clone()).ok())
                else {
                    return Err("打码需要指定区域".into());
                };
                let Some(mode) = MaskMode::parse(
                    params.get("mode").and_then(|v| v.as_str()).unwrap_or("mosaic"),
                ) else {
                    return Err("未知的打码方式".into());
                };
                Ok(Action::Mask(Mask { rect: rect.clean(), mode, strength: num("strength", 16.0) }))
            }
            _ => Err(format!("未知的操作类型：{action}")),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Action::Rotate(op) => op.label().to_string(),
            Action::Crop(c) => c.label().to_string(),
            Action::Tone(t) => t.label(),
            Action::Mask(m) => m.label(),
            Action::Raw => "原图".to_string(),
        }
    }

    /// 会不会改变图片尺寸（决定索引里的宽高要不要更新）。
    pub fn changes_size(&self) -> bool {
        match self {
            Action::Rotate(op) => matches!(op, Op::Rot90 | Op::Rot270),
            Action::Crop(c) => c.aspect.is_some() || c.rect.is_some(),
            Action::Tone(_) => false,
            Action::Mask(_) => false,
            Action::Raw => false,
        }
    }
    pub fn apply(&self, img: RgbaImage) -> RgbaImage {
        match self {
            Action::Rotate(op) => op.apply(img),
            Action::Crop(c) => c.apply(img),
            Action::Tone(t) => t.apply(&img),
            Action::Mask(m) => m.apply(&img),
            Action::Raw => img,
        }
    }
}

/// 调色预设列表（给前端下拉用）。
pub fn tone_presets() -> serde_json::Value {
    serde_json::json!({
        "presets": TONE_PRESETS.iter().map(|(k, n)| serde_json::json!({"key": k, "name": n})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> RgbaImage {
        let mut i = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // 水平渐变，方便验证裁剪与调色的位置
                i.put_pixel(x, y, image::Rgba([(x * 255 / w.max(1)) as u8, 128, 200, 255]));
            }
        }
        i
    }

    #[test]
    fn op_parse_covers_all() {
        assert_eq!(Op::parse("rot90"), Some(Op::Rot90));
        assert_eq!(Op::parse("ROT180"), Some(Op::Rot180));
        assert_eq!(Op::parse("rot270"), Some(Op::Rot270));
        assert_eq!(Op::parse("fliph"), Some(Op::FlipH));
        assert_eq!(Op::parse("flipv"), Some(Op::FlipV));
        assert_eq!(Op::parse("nope"), None);
    }

    #[test]
    fn rotate90_swaps_dimensions() {
        let o = Op::Rot90.apply(img(4, 2));
        assert_eq!((o.width(), o.height()), (2, 4));
    }

    #[test]
    fn rotate180_keeps_size() {
        let o = Op::Rot180.apply(img(4, 2));
        assert_eq!((o.width(), o.height()), (4, 2));
    }

    #[test]
    fn flip_h_mirrors() {
        let o = Op::FlipH.apply(img(4, 2));
        assert_eq!(o.get_pixel(3, 0), &image::Rgba([0, 128, 200, 255]));
    }

    #[test]
    fn flip_v_mirrors() {
        let o = Op::FlipV.apply(img(4, 2));
        assert_eq!(o.get_pixel(0, 1), &image::Rgba([0, 128, 200, 255]));
    }

    #[test]
    fn rect_pixels_maps_percent() {
        let (x, y, w, h) = Rect::new(50.0, 50.0, 50.0, 50.0).pixels(200, 100);
        assert_eq!((x, y, w, h), (100, 50, 100, 50));
    }

    #[test]
    fn rect_clean_keeps_inside() {
        let r = Rect::new(80.0, 80.0, 50.0, 50.0).clean();
        assert_eq!(r.x + r.w, 100.0);
        assert_eq!(r.y + r.h, 100.0);
    }

    #[test]
    fn rect_pixels_never_zero_size() {
        let (_, _, w, h) = Rect::new(0.0, 0.0, 0.1, 0.1).pixels(100, 100);
        assert!(w >= 1 && h >= 1);
    }

    #[test]
    fn crop_by_aspect_square() {
        let o = Crop { rect: None, aspect: Some(1.0), h: HAlign::Center, v: VAlign::Center }.apply(img(200, 100));
        assert_eq!((o.width(), o.height()), (100, 100));
    }

    #[test]
    fn crop_by_aspect_aligns() {
        let o = Crop { rect: None, aspect: Some(1.0), h: HAlign::Left, v: VAlign::Top }.apply(img(200, 100));
        // 左上角对齐时，取到的应是原图左上 100x100
        assert_eq!(o.get_pixel(0, 0), img(200, 100).get_pixel(0, 0));
    }

    #[test]
    fn crop_by_rect_keeps_region() {
        let src = img(100, 100);
        let o = Crop { rect: Some(Rect::new(10.0, 10.0, 50.0, 50.0)), aspect: None, h: HAlign::Center, v: VAlign::Center }.apply(src.clone());
        assert_eq!((o.width(), o.height()), (50, 50));
        assert_eq!(o.get_pixel(0, 0), src.get_pixel(10, 10));
    }

    #[test]
    fn tone_mono_is_grayscale() {
        let o = Tone { preset: "mono".into(), ..Default::default() }.apply(&img(20, 20));
        let p = *o.get_pixel(5, 5);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn tone_brightness_lightens() {
        let src = img(20, 20);
        let before = src.get_pixel(0, 0)[0];
        let o = Tone { brightness: 30.0, ..Default::default() }.apply(&src);
        assert!(o.get_pixel(0, 0)[0] > before);
    }

    #[test]
    fn tone_contrast_pushes_apart() {
        let src = img(20, 20);
        let o = Tone { contrast: 100.0, ..Default::default() }.apply(&src);
        assert_ne!(*o.get_pixel(0, 0), *src.get_pixel(0, 0));
    }

    #[test]
    fn tone_noop_returns_identical() {
        let src = img(8, 8);
        let o = Tone::default().apply(&src);
        assert_eq!(o.as_raw(), src.as_raw());
    }

    #[test]
    fn tone_warm_shifts_red_up() {
        let src = img(8, 8);
        let o = Tone { warmth: 60.0, ..Default::default() }.apply(&src);
        assert!(o.get_pixel(0, 0)[0] >= src.get_pixel(0, 0)[0]);
        assert!(o.get_pixel(0, 0)[2] <= src.get_pixel(0, 0)[2]);
    }

    #[test]
    fn mask_mosaic_flattens_region() {
        let mut src = img(40, 40);
        // 左上角放一个纯红块
        for y in 0..10 {
            for x in 0..10 {
                src.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let o = Mask {
            rect: Rect::new(0.0, 0.0, 25.0, 25.0),
            mode: MaskMode::Mosaic,
            strength: 8.0,
        }
        .apply(&src);
        // 打码区内颜色应趋于一致
        assert_eq!(o.get_pixel(0, 0), o.get_pixel(4, 4));
    }

    #[test]
    fn mask_blur_only_touches_region() {
        let mut src = img(40, 40);
        // 左半边纯红、右半边纯蓝，制造明确的边界，模糊后边界处会被混色
        for y in 0..40 {
            for x in 20..40 {
                src.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let o = Mask { rect: Rect::new(0.0, 0.0, 50.0, 50.0), mode: MaskMode::Blur, strength: 6.0 }.apply(&src);
        // 区域外仍是纯红（打码区只覆盖左半边）
        assert_eq!(*o.get_pixel(35, 35), image::Rgba([0, 0, 255, 255]));
        // 区域内靠近边界的地方被混色了
        assert_ne!(*o.get_pixel(19, 20), image::Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn action_from_params_crop() {
        let p = serde_json::json!({"rect": {"x": 1.0, "y": 2.0, "w": 3.0, "h": 4.0}});
        let a = Action::from_params("", "crop", &p).unwrap();
        match a {
            Action::Crop(c) => assert_eq!(c.rect.unwrap(), Rect::new(1.0, 2.0, 3.0, 4.0)),
            _ => panic!("应为裁剪"),
        }
    }

    #[test]
    fn action_from_params_legacy_op() {
        let a = Action::from_params("rot90", "", &serde_json::json!({})).unwrap();
        assert_eq!(a, Action::Rotate(Op::Rot90));
    }

    #[test]
    fn action_from_params_rejects_unknown() {
        assert!(Action::from_params("", "nope", &serde_json::json!({})).is_err());
        assert!(Action::from_params("", "crop", &serde_json::json!({})).is_err());
        assert!(Action::from_params("", "tone", &serde_json::json!({"preset": "zzz"})).is_err());
    }

    #[test]
    fn action_changes_size_flags() {
        assert!(Action::Rotate(Op::Rot90).changes_size());
        assert!(!Action::Rotate(Op::Rot180).changes_size());
        // 空裁剪（既没有区域也没有比例）什么都不做
        assert!(!Action::Crop(Crop::default()).changes_size());
        // 指定了比例或区域才会裁
        assert!(Action::Crop(Crop { aspect: Some(1.0), ..Crop::default() }).changes_size());
        assert!(Action::Crop(Crop { rect: Some(Rect::new(0.0, 0.0, 50.0, 50.0)), ..Crop::default() }).changes_size());
        assert!(!Action::Tone(Tone::default()).changes_size());
        assert!(!Action::Mask(Mask::default()).changes_size());
    }
}
