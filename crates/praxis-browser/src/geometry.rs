//! Screenshot scaling metadata and image⇄CSS coordinate mapping.
//! **Pure image + arithmetic; no CDP or browser types.**
//!
//! Mirrors `core/_image_cap.py` (metadata write/read) and
//! `core/mouse._scale_coords` (image pixel → CSS viewport). The metadata
//! travels *inside* the PNG as a `tEXt` chunk keyed `ai_dev_browser`, so a
//! screenshot produced by either implementation is readable by the other.

use std::io::BufWriter;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// PNG text-chunk key shared with the Python implementation.
pub const METADATA_KEY: &str = "ai_dev_browser";

/// Default long-edge cap (Claude's effective visual resolution).
pub const MAX_SCREENSHOT_LONG_EDGE: u32 = 1280;
/// Default total-pixel cap (Claude API constraint).
pub const MAX_SCREENSHOT_TOTAL_PIXELS: u64 = 1_150_000;

/// What a screenshot embeds so coordinates read off the image can be mapped
/// back to CSS viewport space. Field order matches Python's `json.dumps`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotMeta {
    /// CSS px per image px: `css = image * scale_factor`.
    pub scale_factor: f64,
    /// `window.innerWidth` at capture time.
    pub viewport_width: u32,
    /// `window.innerHeight` at capture time.
    pub viewport_height: u32,
    /// Final image width.
    pub image_width: u32,
    /// Final image height.
    pub image_height: u32,
    /// `window.devicePixelRatio` at capture time.
    pub device_pixel_ratio: f64,
}

/// Map an image-space point back to CSS viewport space.
#[must_use]
pub fn scale_coords(x: f64, y: f64, meta: Option<&ScreenshotMeta>) -> (f64, f64) {
    match meta {
        Some(m) if (m.scale_factor - 1.0).abs() > f64::EPSILON => {
            (x * m.scale_factor, y * m.scale_factor)
        }
        _ => (x, y),
    }
}

/// Target image size after the Python `page_screenshot` scaling ladder:
/// DPR normalisation → `max_long_edge` → `max_total_pixels`. Each step uses
/// `int()` truncation, exactly as the reference does. `0` disables a cap.
#[must_use]
pub fn fit_dimensions(
    orig_width: u32,
    orig_height: u32,
    device_pixel_ratio: f64,
    max_long_edge: u32,
    max_total_pixels: u64,
) -> (u32, u32) {
    let mut w = orig_width;
    let mut h = orig_height;
    if device_pixel_ratio > 1.0 {
        w = (f64::from(orig_width) / device_pixel_ratio) as u32;
        h = (f64::from(orig_height) / device_pixel_ratio) as u32;
    }
    if max_long_edge > 0 {
        let long_edge = w.max(h);
        if long_edge > max_long_edge {
            let ratio = f64::from(max_long_edge) / f64::from(long_edge);
            w = (f64::from(w) * ratio) as u32;
            h = (f64::from(h) * ratio) as u32;
        }
    }
    if max_total_pixels > 0 {
        let total = u64::from(w) * u64::from(h);
        if total > max_total_pixels {
            let ratio = (max_total_pixels as f64 / total as f64).sqrt();
            w = (f64::from(w) * ratio) as u32;
            h = (f64::from(h) * ratio) as u32;
        }
    }
    (w, h)
}

/// Round half to even, like Python's built-in `round()`.
#[must_use]
pub fn py_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// Decode a PNG from bytes into RGBA8.
pub fn decode_png(bytes: &[u8]) -> Result<image::RgbaImage> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|img| img.to_rgba8())
        .map_err(|e| Error::Image(e.to_string()))
}

/// Resize with Lanczos3 (PIL's `LANCZOS` equivalent). No-op if same size.
#[must_use]
pub fn resize(img: &image::RgbaImage, width: u32, height: u32) -> image::RgbaImage {
    if img.width() == width && img.height() == height {
        return img.clone();
    }
    image::imageops::resize(img, width, height, image::imageops::FilterType::Lanczos3)
}

/// Encode `img` as a plain PNG (no metadata) at `path`.
pub fn write_png(path: &Path, img: &image::RgbaImage) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), img.width(), img.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::Image(e.to_string()))?;
    writer
        .write_image_data(img.as_raw())
        .map_err(|e| Error::Image(e.to_string()))?;
    writer.finish().map_err(|e| Error::Image(e.to_string()))?;
    Ok(())
}

/// Encode `img` as PNG with the metadata `tEXt` chunk and write it to `path`.
pub fn write_png_with_metadata(
    path: &Path,
    img: &image::RgbaImage,
    meta: &ScreenshotMeta,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), img.width(), img.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk(METADATA_KEY.to_string(), serde_json::to_string(meta)?)
        .map_err(|e| Error::Image(e.to_string()))?;
    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::Image(e.to_string()))?;
    writer
        .write_image_data(img.as_raw())
        .map_err(|e| Error::Image(e.to_string()))?;
    writer.finish().map_err(|e| Error::Image(e.to_string()))?;
    Ok(())
}

/// Read the metadata back from a PNG. `None` if the file has no
/// `ai_dev_browser` chunk or it does not parse; errors only on I/O.
pub fn read_png_metadata(path: &Path) -> Result<Option<ScreenshotMeta>> {
    let file = std::fs::File::open(path)?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let reader = decoder
        .read_info()
        .map_err(|e| Error::Image(e.to_string()))?;
    let info = reader.info();
    let mut raw: Option<String> = None;
    for chunk in &info.uncompressed_latin1_text {
        if chunk.keyword == METADATA_KEY {
            raw = Some(chunk.text.clone());
        }
    }
    if raw.is_none() {
        for chunk in &info.compressed_latin1_text {
            if chunk.keyword == METADATA_KEY {
                raw = chunk.get_text().ok();
            }
        }
    }
    if raw.is_none() {
        for chunk in &info.utf8_text {
            if chunk.keyword == METADATA_KEY {
                raw = chunk.get_text().ok();
            }
        }
    }
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_dimensions_matches_python_ladder() {
        // 1600x950 @ dpr 1 → long edge 1600 > 1280 → ratio 0.8 → 1280x760
        assert_eq!(fit_dimensions(1600, 950, 1.0, 1280, 1_150_000), (1280, 760));
        // Retina: 3200x1900 @ dpr 2 → 1600x950 → 1280x760
        assert_eq!(
            fit_dimensions(3200, 1900, 2.0, 1280, 1_150_000),
            (1280, 760)
        );
        // Caps disabled
        assert_eq!(fit_dimensions(3000, 2000, 1.0, 0, 0), (3000, 2000));
        // Total-pixel cap: 1280x1280 = 1.64M > 1.15M → sqrt ratio → 1072x1072
        assert_eq!(
            fit_dimensions(1280, 1280, 1.0, 1280, 1_150_000),
            (1072, 1072)
        );
    }

    #[test]
    fn py_round_is_half_even() {
        assert_eq!(py_round(0.5), 0);
        assert_eq!(py_round(1.5), 2);
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(-0.5), 0);
        assert_eq!(py_round(3.7), 4);
    }

    #[test]
    fn scale_coords_applies_factor() {
        let meta = ScreenshotMeta {
            scale_factor: 1.25,
            viewport_width: 1600,
            viewport_height: 950,
            image_width: 1280,
            image_height: 760,
            device_pixel_ratio: 1.0,
        };
        assert_eq!(scale_coords(100.0, 40.0, Some(&meta)), (125.0, 50.0));
        assert_eq!(scale_coords(100.0, 40.0, None), (100.0, 40.0));
    }

    #[test]
    fn png_metadata_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 2, 3, 255]));
        let meta = ScreenshotMeta {
            scale_factor: 1.25,
            viewport_width: 1600,
            viewport_height: 950,
            image_width: 4,
            image_height: 3,
            device_pixel_ratio: 1.0,
        };
        write_png_with_metadata(&path, &img, &meta).unwrap();
        let back = read_png_metadata(&path).unwrap().unwrap();
        assert_eq!(back, meta);
        // Still a valid PNG for a plain decoder
        let decoded = decode_png(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(decoded.dimensions(), (4, 3));
    }

    #[test]
    fn metadata_json_shape_matches_python() {
        let meta = ScreenshotMeta {
            scale_factor: 1.0,
            viewport_width: 1600,
            viewport_height: 950,
            image_width: 1600,
            image_height: 950,
            device_pixel_ratio: 1.0,
        };
        let s = serde_json::to_string(&meta).unwrap();
        assert!(s.starts_with("{\"scale_factor\":1.0,\"viewport_width\":1600"));
    }
}
