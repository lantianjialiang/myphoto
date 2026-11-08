//! 网页相册导出：把选中的照片导成一个自包含文件夹，双击 `index.html` 就能离线浏览。
//!
//! 隐私：导出的图片一律重新编码（不带 EXIF/GPS），只有显式勾选「附带原图」时
//! 才会复制原文件——此时位置信息随原图一起被带出去，界面会提前警告。

use crate::assets;
use crate::jobs::JobCtl;
use crate::media;
use crate::util;
use serde::Deserialize;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(default)]
pub struct AlbumRequest {
    pub ids: Vec<String>,
    pub out_dir: String,
    pub title: String,
    /// 大图长边像素
    pub max_side: u32,
    /// 缩略图长边像素
    pub thumb_side: u32,
    pub include_originals: bool,
    pub quality: u8,
    /// 导出完顺便打包成 <目录名>.zip，方便直接发给别人
    pub zip: bool,
    /// 相册页面内照片的密码（AES-256-GCM）；ZIP 容器本身不加密
    pub password: String,
    /// 页面主题（见 THEMES）
    pub theme: String,
    /// 导出时使用水印页的配置，只影响导出图片。
    pub watermark: Option<crate::watermark::Config>,
}

impl Default for AlbumRequest {
    fn default() -> Self {
        AlbumRequest {
            ids: Vec::new(),
            out_dir: String::new(),
            title: "我的相册".into(),
            max_side: 1600,
            thumb_side: 480,
            include_originals: false,
            quality: 84,
            zip: false,
            password: String::new(),
            theme: "dark".into(),
            watermark: None,
        }
    }
}

fn encode_jpeg(img: &image::RgbaImage, max_side: u32, quality: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let (w, h) = img.dimensions();
    let scale = (max_side as f32 / w.max(h) as f32).min(1.0);
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let resized = if nw == w && nh == h {
        img.clone()
    } else {
        image::imageops::resize(img, nw, nh, image::imageops::FilterType::Lanczos3)
    };
    let rgb = image::DynamicImage::ImageRgba8(resized).to_rgb8();
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(50, 95));
    enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("编码失败：{e}"))?;
    Ok((buf, rgb.width(), rgb.height()))
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "photo".into());
    util::sanitize_segment(&base)
}

// 相册密码保护：用 AES-256-GCM 加密每个图片文件，密钥由密码经 PBKDF2 派生，
// 密码本身不写入任何文件；前端 index.html 在浏览器里用 Web Crypto 解密。
const PBKDF2_ITERS: u32 = 200_000;

fn b64_encode(data: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(data)
}

/// 由密码 + 盐派生出 32 字节 AES-256 密钥。
fn derive_key(password: &str, salt: &[u8; 16]) -> [u8; 32] {
    use hmac::Hmac;
    use pbkdf2::pbkdf2;
    use sha2::Sha256;
    let mut key = [0u8; 32];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, PBKDF2_ITERS, &mut key)
        .expect("PBKDF2 派生密钥失败");
    key
}

/// AES-256-GCM 加密，返回密文（含认证标签）。
fn aes_encrypt(key: &[u8; 32], plaintext: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|e| format!("加密失败：{e}"))
}

/// 生成随机字节（用于盐与每个文件的 nonce）。
fn random_bytes(buf: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(buf).map_err(|e| format!("生成随机数失败：{e}"))
}


/// 相册主题。骨架共用，靠 CSS 变量与几处覆盖样式区分。
pub struct Theme {
    pub key: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    /// 页面骨架里的 CSS 变量
    pub vars: &'static str,
    /// 追加的样式（覆盖骨架里的默认规则）
    pub extra_css: &'static str,
}

/// 可选主题列表（给前端下拉用）。
pub const THEMES: &[Theme] = &[
    Theme {
        key: "dark",
        name: "深色瀑布",
        desc: "深色背景、方形网格，点开有大图浏览",
        vars: "--bg:#0e1015;--fg:#e8ebf2;--dim:#98a1b3;--card:#171a21;--line:rgba(255,255,255,.08);--accent:#7ea6ff;",
        extra_css: "",
    },
    Theme {
        key: "light",
        name: "浅色画廊",
        desc: "白底，适合打印或投影展示",
        vars: "--bg:#f7f7f5;--fg:#1f2328;--dim:#6b7280;--card:#ffffff;--line:rgba(0,0,0,.08);--accent:#2563eb;",
        extra_css: "figure { box-shadow: 0 1px 3px rgba(0,0,0,.08); } .lb { background: rgba(255,255,255,.97); } .lb .cap { color:#1f2328; } .lb .cap a { color:#2563eb; } .lb button { background: rgba(0,0,0,.06); color:#1f2328; } .lb button:hover { background: rgba(0,0,0,.12); }",
    },
    Theme {
        key: "film",
        name: "胶片棕调",
        desc: "暖棕色调，像老照片",
        vars: "--bg:#241d16;--fg:#efe4d4;--dim:#b09a80;--card:#2f261d;--line:rgba(255,220,170,.12);--accent:#e0a869;",
        extra_css: "figure { border-radius: 2px; filter: sepia(.28) saturate(.92); } figcaption { font-family: Georgia, serif; }",
    },
    Theme {
        key: "polaroid",
        name: "拍立得",
        desc: "白边相纸，照片略歪着贴",
        vars: "--bg:#1b1b1d;--fg:#ededed;--dim:#9a9a9e;--card:#fbfbfb;--line:rgba(255,255,255,.10);--accent:#e4b363;",
        extra_css: "figure { background:#fbfbfb; border-radius:2px; padding:8px 8px 30px; transform: rotate(-1deg); } figure:nth-child(3n) { transform: rotate(1.2deg); } figure:nth-child(3n+1) { transform: rotate(-.6deg); } figure img { border-radius:1px; } figcaption { background:transparent; color:#3a3a3a; padding:8px 4px 0; font-size:11px; }",
    },
    Theme {
        key: "mosaic",
        name: "密铺网格",
        desc: "照片按原比例排布，铺得更满",
        vars: "--bg:#12141a;--fg:#e8ebf2;--dim:#98a1b3;--card:#171a21;--line:rgba(255,255,255,.08);--accent:#7ea6ff;",
        extra_css: ".grid { grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 8px; } figure { aspect-ratio: auto; } figure img { height: auto; }",
    },
];

pub fn theme_list() -> Value {
    json!({
        "themes": THEMES.iter().map(|t| json!({
            "key": t.key, "name": t.name, "desc": t.desc,
        })).collect::<Vec<_>>(),
    })
}

fn theme_by_key(key: &str) -> &'static Theme {
    THEMES.iter().find(|t| t.key == key).unwrap_or(&THEMES[0])
}

const TEMPLATE: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>__TITLE__</title>
<style>
  :root { color-scheme: light dark;
          --bg:#0e1015; --fg:#e8ebf2; --dim:#98a1b3;
          --card:#171a21; --line:rgba(255,255,255,.08); --accent:#7ea6ff; }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--fg);
         font: 14px/1.6 "Microsoft YaHei UI", "Segoe UI", system-ui, sans-serif; }
  header { padding: 26px 28px 12px; }
  header h1 { margin: 0 0 4px; font-size: 24px; letter-spacing: .5px; }
  header p { margin: 0; color: var(--dim); font-size: 12.5px; }
  .note { margin: 14px 28px 0; padding: 10px 14px; border-radius: 8px;
          background: rgba(242,169,59,.12); border: 1px solid rgba(242,169,59,.35);
          color: #f2c98a; font-size: 12.5px; }
  .grid { display: grid; gap: 10px; padding: 20px 28px 40px;
          grid-template-columns: repeat(auto-fill, minmax(210px, 1fr)); }
  figure { margin: 0; position: relative; border-radius: 8px; overflow: hidden;
           background: var(--card); cursor: zoom-in; aspect-ratio: 1/1; }
  figure img { width: 100%; height: 100%; object-fit: cover; display: block;
               transition: transform .25s ease, opacity .3s; opacity: 0; }
  figure img.loaded { opacity: 1; }
  figure:hover img { transform: scale(1.035); }
  figcaption { position: absolute; left: 0; right: 0; bottom: 0; padding: 18px 10px 7px;
               font-size: 11.5px; color: #fff; background: linear-gradient(transparent, rgba(0,0,0,.7)); }
  .lb { position: fixed; inset: 0; background: rgba(6,8,12,.96); display: none;
        align-items: center; justify-content: center; flex-direction: column; z-index: 9; }
  .lb.on { display: flex; }
  .lb img { max-width: 94vw; max-height: 84vh; object-fit: contain; }
  .lb .cap { margin-top: 12px; color: #cfd6e4; font-size: 13px; text-align: center; }
  .lb .cap a { color: var(--accent); }
  .lb button { position: absolute; background: rgba(255,255,255,.08); border: 0; color: #fff;
               font-size: 26px; width: 52px; height: 52px; border-radius: 50%; cursor: pointer; }
  .lb button:hover { background: rgba(255,255,255,.18); }
  .lb .prev { left: 18px; top: 50%; transform: translateY(-50%); }
  .lb .next { right: 18px; top: 50%; transform: translateY(-50%); }
  .lb .close { right: 18px; top: 18px; font-size: 22px; }
  footer { padding: 0 28px 30px; color: var(--dim); font-size: 12px; }
  @media (max-width: 520px) {
    .grid { grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); padding: 14px; }
    header { padding: 18px 14px 8px; }
  }
  .pw-gate { position: fixed; inset: 0; z-index: 20; display: none; align-items: center;
             justify-content: center; background: rgba(6,8,12,.97); }
  .pw-card { background: var(--card); border: 1px solid var(--line); border-radius: 14px;
             padding: 30px 28px; width: min(360px, 88vw); text-align: center; }
  .pw-card h2 { margin: 0 0 6px; font-size: 20px; }
  .pw-card p { margin: 0 0 16px; color: var(--dim); font-size: 13px; }
  .pw-card input { width: 100%; padding: 10px 12px; border-radius: 8px; border: 1px solid var(--line);
                   background: #0e1015; color: var(--fg); font-size: 14px; box-sizing: border-box; }
  .pw-card button { margin-top: 14px; padding: 9px 22px; border: 0; border-radius: 8px;
                    background: var(--accent); color: #07101f; font-size: 14px; cursor: pointer; }
  .pw-err { margin-top: 12px; color: #ff8a8a; font-size: 12.5px; min-height: 16px; }
  __VARS__
  __EXTRA__
</style>
</head>
<body>
<header>
  <h1>__TITLE__</h1>
  <p>共 __COUNT__ 张照片 · __RANGE__ · 导出于 __GENERATED__</p>
</header>
__NOTE__
<main class="grid" id="grid"></main>
<footer>由「光匣 · 本地影像管家」导出。此页面不联网，图片与页面在同一文件夹内。</footer>
<div class="lb" id="lb">
  <button class="close" data-act="close" title="关闭（Esc）">×</button>
  <button class="prev" data-act="prev" title="上一张（←）">‹</button>
  <img id="lb-img" alt="">
  <div class="cap" id="lb-cap"></div>
  <button class="next" data-act="next" title="下一张（→）">›</button>
</div>
<div class="pw-gate" id="pw-gate">
  <div class="pw-card">
    <h2>此相册已加密</h2>
    <p>输入密码以查看照片（密码不随文件保存）。</p>
    <input type="password" id="pw-input" placeholder="相册密码" autocomplete="off">
    <button id="pw-go">查看</button>
    <div class="pw-err" id="pw-err"></div>
  </div>
</div>
<script>
const PHOTOS = __DATA__;
const ALBUM_ENC = "__ENC__";
const ALBUM_SALT = "__SALT__";
const PBKDF2_ITERS = __ITERS__;
const grid = document.getElementById('grid');
const box = document.getElementById('lb');
const lbImg = document.getElementById('lb-img');
const lbCap = document.getElementById('lb-cap');
let cur = 0;
function srcOf(p, kind){ return p['_' + kind] || p[kind]; }
function buildGrid(){
  for (let i = 0; i < PHOTOS.length; i++) {
    const p = PHOTOS[i];
    const fig = document.createElement('figure');
    const img = document.createElement('img');
    img.loading = 'lazy';
    img.src = srcOf(p, 'thumb');
    img.alt = p.n;
    img.addEventListener('load', () => img.classList.add('loaded'));
    const cap = document.createElement('figcaption');
    cap.textContent = p.t || '';
    fig.appendChild(img); fig.appendChild(cap);
    fig.addEventListener('click', () => open(i));
    grid.appendChild(fig);
  }
}
function open(i) {
  cur = (i + PHOTOS.length) % PHOTOS.length;
  const p = PHOTOS[cur];
  lbImg.src = srcOf(p, 'large');
  lbCap.innerHTML = (p.t ? p.t + ' · ' : '') + (p.orig
      ? '<a href="' + p.orig + '" download>下载原图</a>'
      : p.n);
  box.classList.add('on');
  document.body.style.overflow = 'hidden';
}
function closeLb() { box.classList.remove('on'); document.body.style.overflow = ''; }
box.addEventListener('click', ev => {
  const act = ev.target.dataset && ev.target.dataset.act;
  if (act === 'prev') open(cur - 1);
  else if (act === 'next') open(cur + 1);
  else if (act === 'close' || ev.target === box) closeLb();
});
document.addEventListener('keydown', ev => {
  if (!box.classList.contains('on')) return;
  if (ev.key === 'Escape') closeLb();
  else if (ev.key === 'ArrowLeft') open(cur - 1);
  else if (ev.key === 'ArrowRight') open(cur + 1);
});
// 密码保护：AES-256-GCM 解密（密钥由密码经 PBKDF2 派生，密码不写入文件）
function b64ToBytes(b64){ const bin = atob(b64); const u = new Uint8Array(bin.length); for (let i=0;i<bin.length;i++) u[i]=bin.charCodeAt(i); return u; }
async function deriveKey(password, salt){
  const enc = new TextEncoder();
  const pwKey = await crypto.subtle.importKey('raw', enc.encode(password), 'PBKDF2', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name:'PBKDF2', salt: salt, iterations: PBKDF2_ITERS, hash:'SHA-256' },
    pwKey, { name:'AES-GCM', length:256 }, false, ['decrypt']
  );
}
async function decryptData(b64, nonceB64, key){
  const ct = b64ToBytes(b64);
  const pt = await crypto.subtle.decrypt({ name:'AES-GCM', iv: b64ToBytes(nonceB64) }, key, ct);
  return URL.createObjectURL(new Blob([pt]));
}
async function unlock(){
  const pw = document.getElementById('pw-input').value;
  if (!pw) return;
  const err = document.getElementById('pw-err');
  err.textContent = '';
  try {
    const key = await deriveKey(pw, b64ToBytes(ALBUM_SALT));
    for (const p of PHOTOS){
      p._thumb = await decryptData(p.thumbData, p.nonceT, key);
      p._large = await decryptData(p.largeData, p.nonce, key);
    }
    document.getElementById('pw-gate').style.display = 'none';
    buildGrid();
  } catch(e){
    err.textContent = '密码错误或文件已损坏';
  }
}
if (ALBUM_ENC){
  document.getElementById('pw-gate').style.display = 'flex';
  document.getElementById('pw-go').addEventListener('click', unlock);
  document.getElementById('pw-input').addEventListener('keydown', e => { if (e.key === 'Enter') unlock(); });
} else {
  buildGrid();
}
</script>
</body>
</html>
"#;

/// 把导出目录整体打包成同名 zip（放在它旁边），返回 (zip 路径, 打包文件数)。
fn write_zip(root: &Path) -> Result<(PathBuf, usize), String> {
    use std::io::Write;
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "album".into());
    let parent = root.parent().unwrap_or(Path::new("."));
    let mut zip_path = parent.join(format!("{name}.zip"));
    for i in 2..1000 {
        if !zip_path.exists() {
            break;
        }
        zip_path = parent.join(format!("{name} ({i}).zip"));
    }
    let file = std::fs::File::create(&zip_path).map_err(|e| format!("无法创建 zip：{e}"))?;
    let mut zw = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut count = 0usize;
    for entry in walkdir::WalkDir::new(root).sort_by_file_name().into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let inner = format!("{}/{}", name, rel.to_string_lossy().replace('\\', "/"));
        zw.start_file(inner, opts).map_err(|e| format!("写入 zip 失败：{e}"))?;
        let data = std::fs::read(entry.path()).map_err(|e| format!("读取文件失败：{e}"))?;
        zw.write_all(&data).map_err(|e| format!("写入 zip 失败：{e}"))?;
        count += 1;
    }
    let mut buf = zw.finish().map_err(|e| format!("收尾 zip 失败：{e}"))?;
    buf.flush().map_err(|e| format!("写入 zip 失败：{e}"))?;
    Ok((zip_path, count))
}

// 每次导出独占一个新目录，避免旧照片进入本次相册或加密压缩包。
fn reserve_output(requested: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = requested.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("无法创建输出目录：{e}"))?;
    }
    let name = requested.file_name().ok_or("请选择相册文件夹，不能直接使用盘符根目录")?.to_string_lossy();
    for i in 1..10000 {
        let candidate = if i == 1 { requested.to_path_buf() }
            else { requested.with_file_name(format!("{name} ({i})")) };
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("无法创建相册目录：{e}")),
        }
    }
    Err("相册目录重名过多，请选择其他目录".into())
}

pub fn run(db_path: &Path, job: &JobCtl, req: AlbumRequest) -> Result<(), String> {
    let out_root = req.out_dir.trim();
    if out_root.is_empty() {
        return Err("没有指定输出目录".into());
    }
    let out_root = reserve_output(Path::new(out_root))?;
    let photos_dir = out_root.join("photos");
    let thumbs_dir = out_root.join("thumbs");
    std::fs::create_dir_all(&photos_dir).map_err(|e| format!("无法创建输出目录：{e}"))?;
    std::fs::create_dir_all(&thumbs_dir).map_err(|e| format!("无法创建输出目录：{e}"))?;
    let conn = crate::db::open(db_path)?;
    let quality = req.quality.clamp(50, 95);
    let max_side = req.max_side.clamp(640, 4000);
    let thumb_side = req.thumb_side.clamp(160, 800).min(max_side);

    let mut items: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut with_gps = 0usize;
    let mut idx = 0usize;
    job.set_total(req.ids.len() as i64);
    job.phase("准备网页相册…");

    // 密码保护：加密时不落单独的图片文件，而是把密文内嵌进 index.html，
    // 由前端用 Web Crypto 解密——这样双击 file:// 打开也能用（浏览器禁止 fetch 本地文件）。
    let encrypted = !req.password.is_empty();
    let mut album_salt: Option<[u8; 16]> = None;
    let mut key: Option<[u8; 32]> = None;
    if encrypted {
        let mut s = [0u8; 16];
        random_bytes(&mut s)?;
        let k = derive_key(&req.password, &s);
        album_salt = Some(s);
        key = Some(k);
    }

    for id in req.ids.iter() {
        if job.is_cancelled() {
            break;
        }
        let row: Option<(String, Option<String>, Option<f64>)> = conn
            .query_row(
                "SELECT media_type, effective_taken_at, gps_lat FROM media_assets WHERE id=?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional().map_err(|e|format!("读取照片索引失败：{e}"))?;
        let Some((media_type, taken, gps)) = row else {
            job.fail();
            if errors.len() < 200 {
                errors.push(format!("{id}：索引里找不到该条目"));
            }
            continue;
        };
        if media_type != "image" {
            job.skip();
            continue;
        }
        let file = match assets::primary_file(&conn, id) {
            Ok(Some(f)) => f,
            _ => {
                job.fail();
                if errors.len() < 200 {
                    errors.push(format!("{id}：没有可用文件"));
                }
                continue;
            }
        };
        if file.availability != "online" {
            job.fail();
            if errors.len() < 200 {
                errors.push(format!("{}：原文件不可用", file.file_name));
            }
            continue;
        }
        if gps.is_some() {
            with_gps += 1;
        }
        idx += 1;
        let stem = format!("{idx:04}");
        let src = PathBuf::from(&file.absolute_path);
        let result = (|| -> Result<Value, String> {
            let mut img = media::load_oriented(&src)?.to_rgba8();
            if let Some(cfg) = &req.watermark {
                img = crate::watermark::render_for_asset(&conn, id, &src, img, cfg)?;
            }
            let (large, lw, lh) = encode_jpeg(&img, max_side, quality)?;
            let (thumb, _, _) = encode_jpeg(&img, thumb_side, quality)?;
            let mut orig = Value::Null;
            if req.include_originals && !encrypted {
                let name = format!("{stem}_{}", sanitize_file_name(&file.file_name));
                let dest = out_root.join("originals").join(&name);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::copy(&src, &dest).map_err(|e| format!("复制原图失败：{e}"))?;
                orig = json!(format!("originals/{}", name.replace('\\', "/")));
            }
            if encrypted {
                let k = key.expect("encrypted 模式下密钥必已派生");
                let mut nonce_l = [0u8; 12];
                random_bytes(&mut nonce_l)?;
                let mut nonce_t = [0u8; 12];
                random_bytes(&mut nonce_t)?;
                let cl = aes_encrypt(&k, &large, &nonce_l)?;
                let ct = aes_encrypt(&k, &thumb, &nonce_t)?;
                Ok(json!({
                    "t": taken.clone().unwrap_or_default().replace('T', " ").chars().take(16).collect::<String>(),
                    "n": file.file_name,
                    "w": lw,
                    "h": lh,
                    "enc": true,
                    "nonce": b64_encode(&nonce_l),
                    "nonceT": b64_encode(&nonce_t),
                    "thumbData": b64_encode(&ct),
                    "largeData": b64_encode(&cl),
                    "orig": orig,
                }))
            } else {
                std::fs::write(photos_dir.join(format!("{stem}.jpg")), &large)
                    .map_err(|e| format!("写入大图失败：{e}"))?;
                std::fs::write(thumbs_dir.join(format!("{stem}.jpg")), &thumb)
                    .map_err(|e| format!("写入缩略图失败：{e}"))?;
                Ok(json!({
                    "t": taken.clone().unwrap_or_default().replace('T', " ").chars().take(16).collect::<String>(),
                    "n": file.file_name,
                    "w": lw,
                    "h": lh,
                    "thumb": format!("thumbs/{stem}.jpg"),
                    "large": format!("photos/{stem}.jpg"),
                    "orig": orig,
                }))
            }
        })();
        match result {
            Ok(v) => {
                items.push(v);
                job.ok();
            }
            Err(e) => {
                job.fail();
                if errors.len() < 200 {
                    errors.push(format!("{}：{e}", file.file_name));
                }
            }
        }
        if idx % 5 == 0 {
            job.phase(&format!("正在导出相册… {}/{}", idx, req.ids.len()));
        }
    }

    if job.is_cancelled() { return Ok(()); }
    if items.is_empty() {
        return Err(format!("没有成功导出任何照片：{}", errors.first().map(String::as_str).unwrap_or("所选项目没有可导出的照片")));
    }
    let title = if req.title.trim().is_empty() { "我的相册".to_string() } else { req.title.trim().to_string() };
    let dates: Vec<&str> = items
        .iter()
        .filter_map(|v| v.get("t").and_then(|t| t.as_str()))
        .filter(|t| t.len() >= 10)
        .collect();
    let range = match (dates.iter().min(), dates.iter().max()) {
        (Some(a), Some(b)) => {
            let a = &a[..10];
            let b = &b[..10];
            if a == b { a.to_string() } else { format!("{a} 至 {b}") }
        }
        _ => "日期未知".to_string(),
    };
    let note = if req.include_originals && !encrypted && with_gps > 0 {
        format!(
            "<div class=\"note\">隐私提示：本相册附带原图，其中 {} 张含有 GPS 位置信息；大图预览已剥离元数据，分享前请确认是否需要原图。</div>",
            with_gps
        )
    } else if req.include_originals && !encrypted {
        "<div class=\"note\">本相册附带原图，分享时请注意原图可能包含拍摄设备等信息。</div>".to_string()
    } else {
        String::new()
    };
    let theme = theme_by_key(req.theme.trim());
    let enc_flag = if encrypted { "1" } else { "" };
    let salt_b64 = match album_salt {
        Some(s) => b64_encode(&s),
        None => String::new(),
    };
    let iters = PBKDF2_ITERS.to_string();
    let html = TEMPLATE
        .replace("__TITLE__", &esc_html(&title))
        .replace("__COUNT__", &items.len().to_string())
        .replace("__RANGE__", &esc_html(&range))
        .replace("__GENERATED__", &esc_html(&util::now_iso().replace('T', " ").chars().take(16).collect::<String>()))
        .replace("__NOTE__", &note)
        .replace("__ENC__", enc_flag)
        .replace("__SALT__", &salt_b64)
        .replace("__ITERS__", &iters)
        .replace("__VARS__", &format!(":root {{ {} }}", theme.vars))
        .replace("__EXTRA__", theme.extra_css)
        .replace("__DATA__", &serde_json::to_string(&items).map_err(|e| e.to_string())?.replace('<', "\\u003c"));
    std::fs::write(out_root.join("index.html"), html).map_err(|e| format!("写入页面失败：{e}"))?;

    let mut zip_note = String::new();
    if req.zip {
        job.phase("正在打包 zip…");
        match write_zip(&out_root) {
            Ok((path, files)) => {
                zip_note = format!("；已打包 {}（{} 个文件）", path.display(), files)
            }
            Err(e) => {
                return Err(format!("相册已保存到 {}，但打包 zip 失败：{e}", out_root.display()));
            }
        }
    }

    let p = job.snapshot();
    let mut msg = format!("相册已导出到 {}：{} 张，失败 {}{}", out_root.display(), p.succeeded, p.failed, zip_note);
    if let Some(first) = errors.first() {
        msg.push_str(&format!("；首个问题：{first}"));
    }
    job.phase(&msg);
    crate::db::log_op(
        &conn,
        "album",
        None,
        Some(&out_root.to_string_lossy()),
        None,
        None,
        if p.failed == 0 { "ok" } else { "partial" },
        Some(&job.id),
        Some(&format!("网页相册 {} 张", p.succeeded)),
    );
    Ok(())
}

#[cfg(test)]
mod crypto_tests {
    use super::*;
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    #[test]
    fn aes_roundtrip_with_pbkdf2_key() {
        let salt = [7u8; 16];
        let key = derive_key("correct horse", &salt);
        let nonce = [3u8; 12];
        let plain = b"a photo's pixels \x00\x01\x02";
        let ct = aes_encrypt(&key, plain, &nonce).unwrap();
        assert_ne!(ct, plain.to_vec());
        // 用同样的密钥/nonce 解密应还原原文（验证前端 Web Crypto 会用到的算法一致）
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let back = cipher
            .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
            .expect("解密应成功");
        assert_eq!(back, plain);
    }

    #[test]
    fn wrong_password_derives_different_key() {
        let salt = [9u8; 16];
        let k1 = derive_key("pw1", &salt);
        let k2 = derive_key("pw2", &salt);
        assert_ne!(k1, k2);
    }

    #[test]
    fn pbkdf2_matches_reference() {
        use hmac::Hmac;
        use pbkdf2::pbkdf2;
        use sha2::Sha256;
        // 与 Node crypto.pbkdf2Sync('passwd', 16字节salt('salt'+0), 1, 32, 'sha256') 对齐
        let mut salt = [0u8; 16];
        salt[..4].copy_from_slice(b"salt");
        let mut key = [0u8; 32];
        pbkdf2::<Hmac<Sha256>>(b"passwd", &salt, 1, &mut key).unwrap();
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "9ea442ea73604e6d898425c54c2f2e108bcdb60b8781f48ee3cd7293fb148f46"
        );
    }
}

