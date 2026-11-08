//! 离线启发式：边缘区域的局部对比度、连通域和文字行排列。
//! 只产生“疑似”提示，不把未检出当作无水印，也不自动写入人工确认标记。
use image::{DynamicImage, GrayImage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

const ALGORITHM: i32 = 1;
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Detection {
    pub suspected: bool,
    pub known: bool,
    /// 启发式特征分，不是统计意义上的概率。
    pub score: u8,
    pub reason: String,
    pub regions: Vec<Region>,
}
#[derive(Clone, Debug)]
struct Component {
    x: usize,
    y: usize,
    right: usize,
    bottom: usize,
    area: usize,
}
impl Component {
    fn w(&self) -> usize {
        self.right - self.x + 1
    }
    fn h(&self) -> usize {
        self.bottom - self.y + 1
    }
    fn cy(&self) -> f32 {
        (self.y + self.bottom) as f32 / 2.0
    }
}

pub fn analyze(image: &DynamicImage) -> Detection {
    let gray = image.thumbnail(1280, 1280).to_luma8();
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let mut best = Detection {
        suspected: false,
        known: false,
        score: 0,
        reason: "未发现明显的边缘文字特征；不能据此确认没有水印".into(),
        regions: vec![],
    };
    if w < 80 || h < 80 {
        return best;
    }
    let mut integral = vec![0u64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut sum = 0;
        for x in 0..w {
            sum += gray.get_pixel(x as u32, y as u32)[0] as u64;
            integral[(y + 1) * (w + 1) + x + 1] = integral[y * (w + 1) + x + 1] + sum;
        }
    }
    for bright in [true, false] {
        let mut mask = vec![false; w * h];
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                if y > h * 23 / 100 && y < h * 72 / 100 && x > w * 17 / 100 && x < w * 83 / 100 {
                    continue;
                }
                let mean = (integral[(y + 3) * (w + 1) + x + 3]
                    + integral[(y - 2) * (w + 1) + x - 2]
                    - integral[(y - 2) * (w + 1) + x + 3]
                    - integral[(y + 3) * (w + 1) + x - 2]) as i32
                    / 25;
                let lum = gray.get_pixel(x as u32, y as u32)[0] as i32;
                mask[y * w + x] = if bright {
                    lum > 140 && lum - mean > 16
                } else {
                    lum < 115 && mean - lum > 16
                };
            }
        }
        let mut components = components(&mut mask, w, h);
        components.sort_by(|a, b| a.cy().total_cmp(&b.cy()));
        let mut rows: Vec<Vec<Component>> = vec![];
        for c in components {
            let row = rows.iter_mut().find(|row| {
                let r = &row[0];
                (r.cy() - c.cy()).abs() < r.h().max(c.h()) as f32 * 0.42
                    && r.h().min(c.h()) as f32 / r.h().max(c.h()) as f32 > 0.45
            });
            if let Some(row) = row {
                row.push(c)
            } else {
                rows.push(vec![c]);
            }
        }
        for mut row in rows {
            if row.len() < 5 {
                continue;
            }
            row.sort_by_key(|c| c.x);
            let mut run = Vec::new();
            for c in row {
                if run.last().is_some_and(|last: &Component| {
                    c.x.saturating_sub(last.right) > last.h().max(c.h()) * 2
                }) {
                    candidate(&gray, &run, &mut best);
                    run.clear();
                }
                run.push(c);
            }
            candidate(&gray, &run, &mut best);
        }
    }
    best.suspected = best.score >= 78;
    if best.suspected {
        best.reason = "图像边缘有尺寸相近、横向成行的高对比度文字状区域；请人工确认".into();
    } else {
        best.regions.clear();
    }
    best
}

fn components(mask: &mut [bool], w: usize, h: usize) -> Vec<Component> {
    let mut result = vec![];
    let mut queue = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] {
            continue;
        }
        mask[start] = false;
        queue.clear();
        queue.push(start);
        let mut c = Component {
            x: start % w,
            y: start / w,
            right: start % w,
            bottom: start / w,
            area: 0,
        };
        let mut head = 0;
        while head < queue.len() {
            let p = queue[head];
            head += 1;
            let (x, y) = (p % w, p / w);
            c.x = c.x.min(x);
            c.right = c.right.max(x);
            c.y = c.y.min(y);
            c.bottom = c.bottom.max(y);
            c.area += 1;
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let q = ny * w + nx;
                    if mask[q] {
                        mask[q] = false;
                        queue.push(q);
                    }
                }
            }
        }
        let density = c.area as f32 / (c.w() * c.h()) as f32;
        if c.h() >= 5
            && c.h() <= h / 9
            && c.w() >= 2
            && c.w() <= c.h() * 3
            && c.area >= 5
            && density > 0.06
            && density < 0.94
        {
            result.push(c);
        }
    }
    result
}

fn candidate(gray: &GrayImage, run: &[Component], best: &mut Detection) {
    if run.len() < 5 || run.len() > 70 {
        return;
    }
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let x = run.iter().map(|c| c.x).min().unwrap();
    let right = run.iter().map(|c| c.right).max().unwrap();
    let y = run.iter().map(|c| c.y).min().unwrap();
    let bottom = run.iter().map(|c| c.bottom).max().unwrap();
    let (width, height) = (right - x + 1, bottom - y + 1);
    if width < w * 7 / 100 || width * 10 < height * 35 || height > h / 9 {
        return;
    }
    if y > h * 23 / 100 && bottom < h * 72 / 100 && x > w * 12 / 100 && right < w * 88 / 100 {
        return;
    }
    let avg = run.iter().map(|c| c.h()).sum::<usize>() as f32 / run.len() as f32;
    let regular = run
        .iter()
        .filter(|c| c.h() as f32 > avg * 0.65 && (c.h() as f32) < avg * 1.55)
        .count() as f32
        / run.len() as f32;
    if regular < 0.75 {
        return;
    }
    let density = run.iter().map(|c| c.area).sum::<usize>() as f32 / (width * height) as f32;
    if !(0.035..0.64).contains(&density) {
        return;
    }
    let edge_distance = y.min(h - 1 - bottom) as f32 / h as f32;
    // 树叶、栅栏等密集纹理在候选行周围也有大量边缘，降低这类误报。
    let pad = avg.max(4.0) as usize * 2;
    let mut sampled = 0;
    let mut edges = 0;
    for py in (y.saturating_sub(pad)..(bottom + pad).min(h - 1)).step_by(2) {
        for px in (x.saturating_sub(pad)..(right + pad).min(w - 1)).step_by(2) {
            if px >= x && px <= right && py >= y && py <= bottom {
                continue;
            }
            let a = gray.get_pixel(px as u32, py as u32)[0] as i32;
            let dx = (a - gray.get_pixel((px + 1) as u32, py as u32)[0] as i32).abs();
            let dy = (a - gray.get_pixel(px as u32, (py + 1) as u32)[0] as i32).abs();
            sampled += 1;
            if dx + dy > 40 {
                edges += 1;
            }
        }
    }
    let clutter = if sampled > 0 {
        edges as f32 / sampled as f32
    } else {
        0.0
    };
    let score = (49.0
        + (run.len().min(18) as f32) * 1.25
        + regular * 8.0
        + if edge_distance < 0.12 { 9.0 } else { 2.0 }
        - (clutter - 0.15).max(0.0) * 40.0)
        .min(94.0) as u8;
    if score > best.score {
        best.score = score;
        best.regions = vec![Region {
            x: x as f32 / w as f32 * 100.0,
            y: y as f32 / h as f32 * 100.0,
            width: width as f32 / w as f32 * 100.0,
            height: height as f32 / h as f32 * 100.0,
        }];
    }
}

pub fn analyze_asset(conn: &Connection, id: &str) -> Result<Detection, String> {
    let file = crate::assets::primary_file(conn, id)?.ok_or("照片文件不存在")?;
    if crate::util::media_kind(&file.ext) != Some("image") {
        return Err("基础水印检测仅支持照片".into());
    }
    let path = Path::new(&file.absolute_path);
    let content_hash = crate::util::sha256_file(path).map_err(|e| e.to_string())?.0;
    if crate::watermark::has_watermark_hash(conn, path, &content_hash)? {
        return Ok(Detection {
            suspected: false,
            known: true,
            score: 100,
            reason: "已有本软件生成记录或人工确认标记".into(),
            regions: vec![],
        });
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|t| t.as_millis() as i64)
        .unwrap_or(0);
    let rotation = crate::orientation::degrees(conn, id);
    let cached:Option<String>=conn.query_row("SELECT result_json FROM watermark_detection WHERE file_id=?1 AND size_bytes=?2 AND mtime_ms=?3 AND rotation=?4 AND algorithm=?5 AND content_hash=?6",params![file.id,meta.len() as i64,stamp,rotation,ALGORITHM,content_hash],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
    if let Some(cached) = cached {
        if let Ok(result) = serde_json::from_str(&cached) {
            return Ok(result);
        }
    }
    let image = crate::orientation::load(conn, id, path)?;
    let result = analyze(&image);
    conn.execute("INSERT OR REPLACE INTO watermark_detection(file_id,size_bytes,mtime_ms,rotation,algorithm,content_hash,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![file.id,meta.len() as i64,stamp,rotation,ALGORITHM,content_hash,serde_json::to_string(&result).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    #[test]
    fn uniform_and_simple_scene_are_not_watermarks() {
        let plain =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(1000, 700, Rgba([90, 130, 180, 255])));
        assert!(!analyze(&plain).suspected);
        let mut scene = plain.to_rgba8();
        for y in 450..700 {
            for x in 0..1000 {
                scene.put_pixel(x, y, Rgba([30, 70, 40, 255]));
            }
        }
        assert!(!analyze(&DynamicImage::ImageRgba8(scene)).suspected);
    }
    #[test]
    fn detects_rendered_bottom_timestamp() {
        let mut cfg = crate::watermark::Config::default();
        cfg.color = "#FFFFFF".into();
        cfg.stroke = 0.0;
        cfg.shadow = 0.0;
        cfg.size_pct = 4.0;
        let img = RgbaImage::from_pixel(1200, 800, Rgba([40, 70, 110, 255]));
        let rendered = crate::watermark::render(img, &cfg, "2024-11-06 21:45");
        let result = analyze(&DynamicImage::ImageRgba8(rendered.img));
        assert!(result.suspected, "{result:?}");
        assert!(result.regions[0].y > 70.0);
    }
    #[test]
    fn center_text_is_not_treated_as_edge_watermark() {
        let mut cfg = crate::watermark::Config::default();
        cfg.position = "mc".into();
        cfg.size_pct = 4.0;
        let img = RgbaImage::from_pixel(1200, 800, Rgba([40, 70, 110, 255]));
        let rendered = crate::watermark::render(img, &cfg, "CENTER SIGNBOARD");
        assert!(!analyze(&DynamicImage::ImageRgba8(rendered.img)).suspected);
    }
    #[test]
    fn dense_random_texture_is_not_a_text_watermark() {
        let mut img = RgbaImage::new(960, 640);
        let mut seed = 12345u32;
        for by in (0..640).step_by(3) {
            for bx in (0..960).step_by(3) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let v = (seed >> 24) as u8;
                for y in by..(by + 3).min(640) {
                    for x in bx..(bx + 3).min(960) {
                        img.put_pixel(x, y, Rgba([v, v / 2, v / 3, 255]));
                    }
                }
            }
        }
        let result = analyze(&DynamicImage::ImageRgba8(img));
        assert!(!result.suspected, "{result:?}");
    }
}
