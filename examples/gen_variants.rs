//! 生成测试用的「相似照片」变体（缩放 / 重新压缩 / 轻微裁剪）。
//!
//! 仅用于测试固件，不参与主程序构建：
//!   cargo run --release --example gen_variants -- <源图> <输出目录>

use std::error::Error;

fn save(img: &image::RgbImage, path: &str, quality: u8) -> Result<(), Box<dyn Error>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    std::fs::write(path, buf)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("用法: gen_variants <源图> <输出目录>");
        std::process::exit(2);
    }
    let (src, out) = (&args[0], &args[1]);
    std::fs::create_dir_all(out)?;
    let base = image::open(src)?.to_rgb8();
    let (w, h) = (base.width(), base.height());

    // 同一张照片的三种“再来一次”：缩小后重压、低质量、轻微裁剪
    let small = image::imageops::resize(&base, w * 9 / 10, h * 9 / 10, image::imageops::FilterType::Triangle);
    save(&small, &format!("{out}/variant-a.jpg"), 82)?;

    let lower = image::imageops::resize(&base, w * 7 / 10, h * 7 / 10, image::imageops::FilterType::CatmullRom);
    save(&lower, &format!("{out}/variant-b.jpg"), 55)?;

    let crop_w = w * 95 / 100;
    let crop_h = h * 95 / 100;
    let crop = image::imageops::crop_imm(&base, w / 40, h / 40, crop_w, crop_h).to_image();
    save(&crop, &format!("{out}/variant-c.jpg"), 90)?;

    // 负例：旋转 90° 后 dHash 结构完全不同，不应与上面几张聚到一组
    let rotated = image::imageops::rotate90(&base);
    save(&rotated, &format!("{out}/not-similar.jpg"), 80)?;

    println!("已生成 4 张测试图到 {out}");
    Ok(())
}
