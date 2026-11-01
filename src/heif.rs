//! HEIC / HEIF 解码：运行时加载 libheif 的 C API（`heif.dll`）。
//!
//! 设计要点：
//! - **不静态链接**：DLL 缺失、版本过旧、加载失败时，这里只返回可读的原因，
//!   扫描、索引、去重、归档等其它功能完全不受影响（HEIC 仍可索引，只是没有缩略图）。
//! - **只认解码所需的最小接口**：context / primary handle / decode / plane / metadata。
//! - **依赖查找**：优先 `PVM_HEIF_DLL` 指定的文件，其次 `<exe>\lib\heif.dll`、
//!   `<exe>\heif.dll`、`<数据目录>\lib\heif.dll`；用 `LOAD_WITH_ALTERED_SEARCH_PATH`
//!   让 `heif.dll` 从自己所在目录找 `libde265.dll`，不需要改系统 PATH。
//! - 只读：只读取源文件字节，不写、不改、不移动原文件。

use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const DLL_NAME: &str = "heif.dll";

// libheif 枚举取值（见 libheif/heif.h）
const COLORSPACE_RGB: c_int = 1;
const CHROMA_INTERLEAVED_RGB: c_int = 10;
const CHANNEL_INTERLEAVED: c_int = 10;

#[repr(C)]
#[derive(Clone, Copy)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

impl HeifError {
    fn text(&self) -> String {
        let msg = if self.message.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(self.message) }
                .to_string_lossy()
                .to_string()
        };
        format!("{msg}（code={}）", self.code)
    }
}

type Ctx = *mut c_void;
type ImageHandle = *mut c_void;
type Image = *mut c_void;

#[cfg(windows)]
mod sys {
    use std::ffi::{c_char, c_void};

    #[link(name = "kernel32")]
    extern "system" {
        pub fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        pub fn FreeLibrary(module: *mut c_void) -> i32;
        pub fn GetLastError() -> u32;
    }
}

struct Api {
    path: PathBuf,
    version: String,
    context_alloc: unsafe extern "C" fn() -> Ctx,
    context_free: unsafe extern "C" fn(Ctx),
    read_memory: unsafe extern "C" fn(Ctx, *const u8, usize, *const c_void) -> HeifError,
    get_primary: unsafe extern "C" fn(Ctx, *mut ImageHandle) -> HeifError,
    handle_width: unsafe extern "C" fn(ImageHandle) -> c_int,
    handle_height: unsafe extern "C" fn(ImageHandle) -> c_int,
    handle_release: unsafe extern "C" fn(ImageHandle),
    decode: unsafe extern "C" fn(ImageHandle, *mut Image, c_int, c_int, *const c_void) -> HeifError,
    plane_read: unsafe extern "C" fn(Image, c_int, *mut c_int) -> *const u8,
    image_release: unsafe extern "C" fn(Image),
    meta_count: unsafe extern "C" fn(ImageHandle, *const c_char) -> c_int,
    meta_ids: unsafe extern "C" fn(ImageHandle, *const c_char, *mut u32, c_int) -> c_int,
    meta_size: unsafe extern "C" fn(ImageHandle, u32) -> usize,
    meta_read: unsafe extern "C" fn(ImageHandle, u32, *mut c_void) -> HeifError,
}

// 函数指针表进程内只读共享；libheif 每个 heif_context 独立，可跨线程使用。
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();
static EXTRA_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 追加 DLL 搜索目录（例如自定义数据目录下的 `lib`）。需在首次使用前调用。
pub fn set_search_dirs(dirs: Vec<PathBuf>) {
    if let Ok(mut slot) = EXTRA_DIRS.lock() {
        *slot = dirs;
    }
}

pub struct Status {
    pub ok: bool,
    pub detail: String,
}

pub fn status() -> Status {
    match api() {
        Ok(a) => Status {
            ok: true,
            detail: format!("{}（libheif {}）", a.path.to_string_lossy(), a.version),
        },
        Err(e) => Status {
            ok: false,
            detail: e.clone(),
        },
    }
}

pub fn available() -> bool {
    api().is_ok()
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(explicit) = std::env::var("PVM_HEIF_DLL") {
        if !explicit.is_empty() {
            out.push(PathBuf::from(explicit));
        }
    }
    if let Ok(mut dirs) = EXTRA_DIRS.lock() {
        for d in dirs.drain(..) {
            out.push(d.join(DLL_NAME));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("lib").join(DLL_NAME));
            out.push(dir.join(DLL_NAME));
        }
    }
    // 从源码目录（cargo build 产物在 target\release）直接运行时也能找到仓库根目录的 lib\
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("lib").join(DLL_NAME));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// 不依赖解码器读取 HEIF 尺寸：解析 `meta/iprp/ipco/ispe`，取面积最大的那个。
/// 解码器缺失时用它给索引补上宽高（网格拼接图取到的就是整图尺寸）。
pub fn fallback_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    fn be32(b: &[u8]) -> Option<u32> {
        b.get(0..4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// 列出容器内的子 box：(类型, 负载起止)
    fn boxes(data: &[u8]) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        let mut o = 0usize;
        while o + 8 <= data.len() {
            let mut size = match be32(&data[o..]) {
                Some(v) => v as usize,
                None => break,
            };
            let kind = String::from_utf8_lossy(&data[o + 4..o + 8]).to_string();
            let mut header = 8usize;
            if size == 1 {
                let big = match data.get(o + 8..o + 16) {
                    Some(v) => v,
                    None => break,
                };
                let mut v = [0u8; 8];
                v.copy_from_slice(big);
                size = u64::from_be_bytes(v) as usize;
                header = 16;
            } else if size == 0 {
                size = data.len() - o;
            }
            if size < header || o + size > data.len() {
                break;
            }
            out.push((kind, o + header, o + size));
            o += size;
        }
        out
    }
    fn find(data: &[u8], want: &str) -> Option<(usize, usize)> {
        boxes(data)
            .into_iter()
            .find(|(k, _, _)| k == want)
            .map(|(_, s, e)| (s, e))
    }

    let (meta_s, meta_e) = find(data, "meta")?;
    let meta = &data[meta_s + 4..meta_e]; // fullbox：跳过 version/flags
    let (iprp_s, iprp_e) = find(meta, "iprp")?;
    let iprp = &meta[iprp_s..iprp_e];
    let (ipco_s, ipco_e) = find(iprp, "ipco")?;
    let ipco = &iprp[ipco_s..ipco_e];

    let mut best: Option<(u32, u32)> = None;
    for (kind, s, e) in boxes(ipco) {
        if kind != "ispe" || s + 12 > e {
            continue;
        }
        let (w, h) = match (be32(&ipco[s + 4..s + 8]), be32(&ipco[s + 8..s + 12])) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => continue,
        };
        let area = w as u64 * h as u64;
        if best
            .map(|(bw, bh)| area > bw as u64 * bh as u64)
            .unwrap_or(true)
        {
            best = Some((w, h));
        }
    }
    best
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(|e| e.clone())
}

#[cfg(windows)]
fn load() -> Result<Api, String> {
    use std::os::windows::ffi::OsStrExt;

    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;

    let tried = candidates();
    for path in &tried {
        if !path.is_file() {
            continue;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let module = unsafe {
            sys::LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
        };
        if module.is_null() {
            continue;
        }
        match unsafe { resolve(module, path.clone()) } {
            Ok(api) => return Ok(api),
            Err(_) => {
                unsafe { sys::FreeLibrary(module) };
            }
        }
    }
    let code = unsafe { sys::GetLastError() };
    if tried.is_empty() {
        return Err("未找到 heif.dll：请把 heif.dll 与 libde265.dll 放到程序目录的 lib\\ 下".into());
    }
    Err(format!(
        "未能加载 heif.dll（Windows 错误码 {code}）。已尝试：{}",
        tried
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("、")
    ))
}

#[cfg(windows)]
unsafe fn resolve(module: *mut c_void, path: PathBuf) -> Result<Api, String> {
    unsafe fn sym(module: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = std::ffi::CString::new(name).unwrap();
        let p = unsafe { sys::GetProcAddress(module, c.as_ptr()) };
        if p.is_null() {
            Err(format!("heif.dll 缺少导出符号 {name}（版本过旧？）"))
        } else {
            Ok(p)
        }
    }

    let need = |name: &str| -> Result<*mut c_void, String> { unsafe { sym(module, name) } };

    let version_fn: unsafe extern "C" fn() -> *const c_char =
        std::mem::transmute(need("heif_get_version")?);
    let version = unsafe { CStr::from_ptr(version_fn()) }
        .to_string_lossy()
        .to_string();

    Ok(Api {
        path,
        version,
        context_alloc: std::mem::transmute(need("heif_context_alloc")?),
        context_free: std::mem::transmute(need("heif_context_free")?),
        read_memory: std::mem::transmute(need("heif_context_read_from_memory_without_copy")?),
        get_primary: std::mem::transmute(need("heif_context_get_primary_image_handle")?),
        handle_width: std::mem::transmute(need("heif_image_handle_get_width")?),
        handle_height: std::mem::transmute(need("heif_image_handle_get_height")?),
        handle_release: std::mem::transmute(need("heif_image_handle_release")?),
        decode: std::mem::transmute(need("heif_decode_image")?),
        plane_read: std::mem::transmute(need("heif_image_get_plane_readonly")?),
        image_release: std::mem::transmute(need("heif_image_release")?),
        meta_count: std::mem::transmute(need("heif_image_handle_get_number_of_metadata_blocks")?),
        meta_ids: std::mem::transmute(need("heif_image_handle_get_list_of_metadata_block_IDs")?),
        meta_size: std::mem::transmute(need("heif_image_handle_get_metadata_size")?),
        meta_read: std::mem::transmute(need("heif_image_handle_get_metadata")?),
    })
}

#[cfg(not(windows))]
fn load() -> Result<Api, String> {
    Err("HEIC 解码目前只在 Windows 上通过 heif.dll 提供".into())
}

/// 一次打开的 HEIF 上下文：context 与 handle 成对释放，提前返回也不泄漏。
struct Session<'a> {
    api: &'static Api,
    ctx: Ctx,
    handle: ImageHandle,
    _data: &'a [u8],
}

impl<'a> Session<'a> {
    fn open(data: &'a [u8]) -> Result<Session<'a>, String> {
        let api = api()?;
        let ctx = unsafe { (api.context_alloc)() };
        if ctx.is_null() {
            return Err("heif_context_alloc 失败（内存不足？）".into());
        }
        let err = unsafe { (api.read_memory)(ctx, data.as_ptr(), data.len(), std::ptr::null()) };
        if err.code != 0 {
            unsafe { (api.context_free)(ctx) };
            return Err(format!("不是有效的 HEIF/HEIC：{}", err.text()));
        }
        let mut handle: ImageHandle = std::ptr::null_mut();
        let err = unsafe { (api.get_primary)(ctx, &mut handle) };
        if err.code != 0 || handle.is_null() {
            unsafe { (api.context_free)(ctx) };
            return Err(format!("读取主图失败：{}", err.text()));
        }
        Ok(Session {
            api,
            ctx,
            handle,
            _data: data,
        })
    }

    fn size(&self) -> (u32, u32) {
        let w = unsafe { (self.api.handle_width)(self.handle) }.max(0) as u32;
        let h = unsafe { (self.api.handle_height)(self.handle) }.max(0) as u32;
        (w, h)
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.api.handle_release)(self.handle);
            (self.api.context_free)(self.ctx);
        }
    }
}

/// 取主图宽高（libheif 会把 irot/imir 旋转属性算进去，所以这是"看到的方向"的尺寸）。
pub fn probe(path: &Path) -> Result<(u32, u32), String> {
    let data = std::fs::read(path).map_err(|e| format!("无法读取文件：{e}"))?;
    let session = Session::open(&data)?;
    let (w, h) = session.size();
    if w == 0 || h == 0 {
        return Err("HEIF 主图尺寸为 0".into());
    }
    Ok((w, h))
}

/// 取出 HEIF 内的 EXIF（TIFF 块），供日期识别使用；没有则返回 None。
pub fn exif_tiff(path: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    let session = Session::open(&data).ok()?;
    let api = session.api;
    let filter = std::ffi::CString::new("Exif").ok()?;
    let count = unsafe { (api.meta_count)(session.handle, filter.as_ptr()) };
    if count <= 0 {
        return None;
    }
    let mut ids = vec![0u32; count as usize];
    let got = unsafe { (api.meta_ids)(session.handle, filter.as_ptr(), ids.as_mut_ptr(), count) };
    if got <= 0 {
        return None;
    }
    let id = ids[0];
    let size = unsafe { (api.meta_size)(session.handle, id) };
    if size == 0 || size > 4 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let err = unsafe { (api.meta_read)(session.handle, id, buf.as_mut_ptr() as *mut c_void) };
    if err.code != 0 {
        return None;
    }
    // HEIF 的 Exif item 前 4 字节是到 TIFF 头的偏移，另有实现会带 "Exif\0\0"，统一切到 TIFF 头。
    let start = find_tiff_header(&buf)?;
    Some(buf[start..].to_vec())
}

fn find_tiff_header(b: &[u8]) -> Option<usize> {
    b.windows(4)
        .position(|w| w == [b'I', b'I', 0x2A, 0x00] || w == [b'M', b'M', 0x00, 0x2A])
}

/// 解码成完整 RGB 图像（已按容器方向校正）。
/// 注意：`imageops::thumbnail` 在目标尺寸大于原图时会**放大**，所以缩略图路径必须先把
/// 目标边长夹到原图长边，否则大图会被放大成上亿像素并触发内存上限。
pub fn decode_rgb(path: &Path) -> Result<image::RgbImage, String> {
    let data = std::fs::read(path).map_err(|e| format!("无法读取文件：{e}"))?;
    let session = Session::open(&data)?;
    let api = session.api;
    let mut image: Image = std::ptr::null_mut();
    let err = unsafe {
        (api.decode)(
            session.handle,
            &mut image,
            COLORSPACE_RGB,
            CHROMA_INTERLEAVED_RGB,
            std::ptr::null(),
        )
    };
    if err.code != 0 || image.is_null() {
        return Err(format!("HEIC 解码失败：{}", err.text()));
    }
    let (w, h) = session.size();
    let mut stride: c_int = 0;
    let plane = unsafe { (api.plane_read)(image, CHANNEL_INTERLEAVED, &mut stride) };
    let result = (|| -> Result<image::RgbImage, String> {
        if plane.is_null() || stride <= 0 || w == 0 || h == 0 {
            return Err("HEIC 解码结果为空".into());
        }
        let stride = stride as usize;
        let row_bytes = w as usize * 3;
        if stride < row_bytes {
            return Err("HEIC 行跨距异常".into());
        }
        let total = stride
            .checked_mul(h as usize)
            .ok_or_else(|| "HEIC 尺寸异常".to_string())?;
        let raw = unsafe { std::slice::from_raw_parts(plane, total) };
        let mut packed = Vec::with_capacity(row_bytes * h as usize);
        for row in 0..h as usize {
            let start = row * stride;
            packed.extend_from_slice(&raw[start..start + row_bytes]);
        }
        image::RgbImage::from_raw(w, h, packed).ok_or_else(|| "HEIC 像素数据不完整".to_string())
    })();
    unsafe { (api.image_release)(image) };
    result
}

/// 解码并生成缩略图 JPEG（最长边不超过 `max_side`）。
pub fn thumbnail_jpeg(path: &Path, max_side: u32) -> Result<Vec<u8>, String> {
    let buffer = decode_rgb(path)?;
    let longest = buffer.width().max(buffer.height());
    let limit = max_side.min(longest).max(1);
    let thumb = if limit >= longest {
        buffer
    } else {
        image::imageops::thumbnail(&buffer, limit, limit)
    };
    let mut out: Vec<u8> = Vec::with_capacity(48 * 1024);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
    enc.encode(
        thumb.as_raw(),
        thumb.width(),
        thumb.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| format!("缩略图编码失败：{e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_never_panics() {
        let s = status();
        assert!(!s.detail.is_empty());
    }

    #[test]
    fn unavailable_message_points_at_lib_folder() {
        if available() {
            return;
        }
        assert!(status().detail.contains("heif.dll"));
    }

    fn mkbox(kind: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind.as_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn reads_size_from_ispe_without_decoder() {
        let ispe = mkbox("ispe", &[0, 0, 0, 0, 0x00, 0x00, 0x0B, 0x40, 0x00, 0x00, 0x04, 0x38]);
        let ipco = mkbox("ipco", &ispe);
        let iprp = mkbox("iprp", &ipco);
        let mut meta_payload = vec![0u8, 0, 0, 0];
        meta_payload.extend(iprp);
        let mut file = mkbox("ftyp", b"heic\0\0\0\0mif1heic");
        file.extend(mkbox("meta", &meta_payload));
        assert_eq!(fallback_dimensions(&file), Some((2880, 1080)));
        assert_eq!(fallback_dimensions(b"not a heif"), None);
    }

    #[test]
    fn finds_tiff_header_inside_heif_exif_block() {        let mut v = vec![0, 0, 0, 10];
        v.extend_from_slice(b"Exif\0\0MM\0\x2a");
        v.extend_from_slice(&[0u8; 8]);
        assert_eq!(find_tiff_header(&v), Some(10));
        assert_eq!(find_tiff_header(b"nope"), None);
    }
}
