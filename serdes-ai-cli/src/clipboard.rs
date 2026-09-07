//! Clipboard integration module.
//!
//! Supports:
//! - Cross-platform image capture with `arboard`
//! - Pending image attachment queue
//! - Optional data URL parsing from text clipboard content
//! - Basic image resizing and BMP encoding for large clipboard images

use std::sync::{Mutex, OnceLock};

use arboard::Clipboard;
use tracing::warn;
use uuid::Uuid;

/// Hard limit for clipboard images before downscaling.
/// Keeps memory usage sane for giant screenshots.
const MAX_PIXELS: usize = 2_000_000; // ~1920x1080

/// Global clipboard manager singleton.
static CLIPBOARD_MANAGER: OnceLock<Mutex<ClipboardManager>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct ClipboardManager {
    pub pending_images: Vec<ClipboardImage>,
}

#[derive(Debug, Clone)]
pub struct ClipboardImage {
    pub id: String,
    pub data: Vec<u8>,
    pub format: ImageFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Bmp,
}

impl ImageFormat {
    #[must_use]
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Bmp => "image/bmp",
        }
    }
}

impl ClipboardImage {
    /// Convert image bytes to base64 for LLM-friendly transport.
    #[must_use]
    pub fn as_base64(&self) -> String {
        base64_encode(&self.data)
    }

    /// Convert image bytes to a data URL (`data:image/...;base64,...`).
    #[must_use]
    pub fn as_data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.format.mime_type(),
            self.as_base64()
        )
    }
}

#[must_use]
pub fn get_clipboard_manager() -> &'static Mutex<ClipboardManager> {
    CLIPBOARD_MANAGER.get_or_init(|| Mutex::new(ClipboardManager::default()))
}

/// Check if clipboard currently contains an image payload.
#[must_use]
pub fn has_image_in_clipboard() -> bool {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(err) => {
            warn!("Clipboard unavailable: {err}");
            return false;
        }
    };

    if clipboard.get_image().is_ok() {
        return true;
    }

    // Fallback: users sometimes copy image data URLs as text.
    match clipboard.get_text() {
        Ok(text) => text.trim_start().starts_with("data:image/"),
        Err(_) => false,
    }
}

/// Capture an image from clipboard if available.
#[must_use]
pub fn capture_clipboard_image() -> Option<ClipboardImage> {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(err) => {
            warn!("Failed to access clipboard: {err}");
            return None;
        }
    };

    if let Ok(image) = clipboard.get_image() {
        return convert_raw_clipboard_image(image.width, image.height, image.bytes.as_ref());
    }

    // Graceful fallback: parse data URL from text clipboard when available.
    let text = match clipboard.get_text() {
        Ok(t) => t,
        Err(err) => {
            warn!("Clipboard has no image and text fallback failed: {err}");
            return None;
        }
    };

    decode_data_url_image(&text)
}

/// Capture image and queue it as pending attachment.
/// Returns placeholder token on success.
#[must_use]
pub fn capture_clipboard_image_to_pending() -> Option<String> {
    let image = capture_clipboard_image()?;
    let id = image.id.clone();

    let manager = get_clipboard_manager();
    let mut guard = manager.lock().ok()?;
    guard.pending_images.push(image);

    Some(format!("[clipboard-image:{id}]"))
}

/// Get all pending images (cloned snapshot).
#[must_use]
pub fn get_pending_images() -> Vec<ClipboardImage> {
    let manager = get_clipboard_manager();
    match manager.lock() {
        Ok(guard) => guard.pending_images.clone(),
        Err(err) => {
            warn!("Clipboard manager lock poisoned while reading pending images: {err}");
            Vec::new()
        }
    }
}

/// Clear pending image queue.
pub fn clear_pending() {
    let manager = get_clipboard_manager();
    match manager.lock() {
        Ok(mut guard) => {
            guard.pending_images.clear();
        }
        _ => {
            warn!("Clipboard manager lock poisoned while clearing pending images");
        }
    }
}

/// Get number of pending images.
#[must_use]
pub fn get_pending_count() -> usize {
    let manager = get_clipboard_manager();
    match manager.lock() {
        Ok(guard) => guard.pending_images.len(),
        Err(err) => {
            warn!("Clipboard manager lock poisoned while counting pending images: {err}");
            0
        }
    }
}

// Backward-compatible wrappers for earlier placeholder API.
#[must_use]
pub fn has_image() -> bool {
    has_image_in_clipboard()
}

#[must_use]
pub fn capture_image() -> Option<Vec<u8>> {
    capture_clipboard_image().map(|img| img.data)
}

fn convert_raw_clipboard_image(
    width: usize,
    height: usize,
    rgba_bytes: &[u8],
) -> Option<ClipboardImage> {
    if width == 0 || height == 0 {
        warn!("Clipboard image had invalid dimensions ({width}x{height})");
        return None;
    }

    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if rgba_bytes.len() < expected {
        warn!(
            "Clipboard image buffer too small: got {}, expected at least {}",
            rgba_bytes.len(),
            expected
        );
        return None;
    }

    let (target_w, target_h, pixels) = maybe_resize_rgba(width, height, &rgba_bytes[..expected]);

    let bmp = encode_bmp_from_rgba(target_w, target_h, &pixels)?;

    Some(ClipboardImage {
        id: Uuid::new_v4().to_string(),
        data: bmp,
        format: ImageFormat::Bmp,
    })
}

fn maybe_resize_rgba(width: usize, height: usize, rgba: &[u8]) -> (usize, usize, Vec<u8>) {
    let pixels = width.saturating_mul(height);
    if pixels <= MAX_PIXELS {
        return (width, height, rgba.to_vec());
    }

    let scale = (MAX_PIXELS as f64 / pixels as f64).sqrt();
    let new_width = ((width as f64 * scale).round() as usize).max(1);
    let new_height = ((height as f64 * scale).round() as usize).max(1);

    let resized = resize_rgba_nearest(rgba, width, height, new_width, new_height);
    (new_width, new_height, resized)
}

fn resize_rgba_nearest(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let mut out = vec![0_u8; dst_w * dst_h * 4];

    for y in 0..dst_h {
        let src_y = y * src_h / dst_h;
        for x in 0..dst_w {
            let src_x = x * src_w / dst_w;

            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = (y * dst_w + x) * 4;

            out[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
        }
    }

    out
}

fn encode_bmp_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    let row_stride = (width * 3 + 3) & !3;
    let pixel_array_size = row_stride.checked_mul(height)?;
    let file_size = 14usize.checked_add(40)?.checked_add(pixel_array_size)?;

    let mut out = Vec::with_capacity(file_size);

    // BMP file header (14 bytes)
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // reserved1
    out.extend_from_slice(&0_u16.to_le_bytes()); // reserved2
    out.extend_from_slice(&(54_u32).to_le_bytes()); // pixel data offset

    // DIB header (BITMAPINFOHEADER, 40 bytes)
    out.extend_from_slice(&(40_u32).to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes()); // bottom-up
    out.extend_from_slice(&(1_u16).to_le_bytes()); // planes
    out.extend_from_slice(&(24_u16).to_le_bytes()); // bits per pixel
    out.extend_from_slice(&(0_u32).to_le_bytes()); // compression BI_RGB
    out.extend_from_slice(&(pixel_array_size as u32).to_le_bytes());
    out.extend_from_slice(&(2835_u32).to_le_bytes()); // x ppm (~72dpi)
    out.extend_from_slice(&(2835_u32).to_le_bytes()); // y ppm
    out.extend_from_slice(&(0_u32).to_le_bytes()); // colors used
    out.extend_from_slice(&(0_u32).to_le_bytes()); // important colors

    // Pixel array: BGR, bottom-up rows, 4-byte aligned rows
    let pad_len = row_stride - (width * 3);
    let pad = vec![0_u8; pad_len];

    for y in (0..height).rev() {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            let r = rgba[idx];
            let g = rgba[idx + 1];
            let b = rgba[idx + 2];
            out.extend_from_slice(&[b, g, r]);
        }
        if pad_len > 0 {
            out.extend_from_slice(&pad);
        }
    }

    Some(out)
}

fn decode_data_url_image(text: &str) -> Option<ClipboardImage> {
    let trimmed = text.trim();
    if !trimmed.starts_with("data:image/") {
        return None;
    }

    let (header, b64_payload) = trimmed.split_once(',')?;
    if !header.ends_with(";base64") {
        warn!("Clipboard data URL is not base64-encoded");
        return None;
    }

    let format = detect_format_from_data_url_header(header)?;
    let data = base64_decode(b64_payload)?;

    Some(ClipboardImage {
        id: Uuid::new_v4().to_string(),
        data,
        format,
    })
}

fn detect_format_from_data_url_header(header: &str) -> Option<ImageFormat> {
    if header.starts_with("data:image/png") {
        Some(ImageFormat::Png)
    } else if header.starts_with("data:image/jpeg") || header.starts_with("data:image/jpg") {
        Some(ImageFormat::Jpeg)
    } else if header.starts_with("data:image/bmp") {
        Some(ImageFormat::Bmp)
    } else {
        warn!("Unsupported image format in clipboard data URL: {header}");
        None
    }
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
        i += 3;
    }

    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }

    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut cleaned = Vec::with_capacity(input.len());
    for b in input.bytes() {
        if !b.is_ascii_whitespace() {
            cleaned.push(b);
        }
    }

    if cleaned.is_empty() || cleaned.len() % 4 != 0 {
        warn!("Invalid base64 input length for clipboard image payload");
        return None;
    }

    let mut out = Vec::with_capacity((cleaned.len() / 4) * 3);

    for chunk in cleaned.chunks_exact(4) {
        let a = b64_val(chunk[0])?;
        let b = b64_val(chunk[1])?;
        let c = if chunk[2] == b'=' {
            64
        } else {
            b64_val(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            64
        } else {
            b64_val(chunk[3])?
        };

        let n = ((a as u32) << 18)
            | ((b as u32) << 12)
            | (((if c == 64 { 0 } else { c }) as u32) << 6)
            | ((if d == 64 { 0 } else { d }) as u32);

        out.push(((n >> 16) & 0xFF) as u8);
        if c != 64 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if d != 64 {
            out.push((n & 0xFF) as u8);
        }
    }

    Some(out)
}

fn b64_val(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(26 + b - b'a'),
        b'0'..=b'9' => Some(52 + b - b'0'),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => {
            warn!("Invalid base64 character in clipboard image payload: {b}");
            None
        }
    }
}
