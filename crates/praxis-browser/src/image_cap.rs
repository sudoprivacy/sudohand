//! Byte / dimension caps for saved screenshots — port of `core/_image_cap.py`.
//!
//! `{"max_bytes", "max_dimension"}`: `max_dimension` resizes so the longest
//! edge fits; `max_bytes` re-encodes as JPEG stepping quality
//! `85 → 70 → 55 → 40 → 25`, halving the dimensions once if even 25 misses.
//! Also owns the metadata write/read for both formats: PNG `tEXt` chunk
//! (byte-compatible with `geometry`) and JPEG EXIF `UserComment` (0x9286),
//! so `mouse_click --screenshot` works on either.

use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageEncoder};
use serde::{Deserialize, Serialize};

use crate::geometry::{self, ScreenshotMeta};
use crate::{Error, Result};

/// `AI_DEV_BROWSER_IMAGE_CAP_MAX_BYTES`.
pub const ENV_MAX_BYTES: &str = "AI_DEV_BROWSER_IMAGE_CAP_MAX_BYTES";
/// `AI_DEV_BROWSER_IMAGE_CAP_MAX_DIMENSION`.
pub const ENV_MAX_DIMENSION: &str = "AI_DEV_BROWSER_IMAGE_CAP_MAX_DIMENSION";
/// JPEG quality ladder.
pub const QUALITY_STEPS: [u8; 5] = [85, 70, 55, 40, 25];
/// Floor for the halving fallback.
pub const MIN_DIMENSION_AFTER_HALVING: u32 = 96;
/// Headroom reserved for the metadata segment when `reserve_bytes_for_metadata`.
pub const METADATA_OVERHEAD_BUDGET: u64 = 500;

const EXIF_USER_COMMENT_PREFIX: &[u8] = b"ASCII\0\0\0";

/// A cap the caller wants a screenshot to fit. Both fields optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCap {
    /// Max file size in bytes (switches output to JPEG).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Max long edge in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_dimension: Option<u32>,
}

impl ImageCap {
    /// True iff at least one constraint is active.
    #[must_use]
    pub fn wants_cap(&self) -> bool {
        self.max_bytes.is_some_and(|b| b > 0) || self.max_dimension.is_some_and(|d| d > 0)
    }

    /// Parse the CLI/JSON form `{"max_bytes": int, "max_dimension": int}`.
    pub fn from_json(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).map_err(|e| Error::Invalid(format!("image_cap: {e}")))
    }
}

/// Precedence: explicit (if it has any constraint) > env vars > `None`.
/// Malformed env values are ignored, never fatal.
#[must_use]
pub fn resolve_cap(explicit: Option<ImageCap>) -> Option<ImageCap> {
    if let Some(c) = explicit {
        if c.wants_cap() {
            return Some(c);
        }
    }
    let env_u64 = |k: &str| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let cap = ImageCap {
        max_bytes: env_u64(ENV_MAX_BYTES),
        max_dimension: env_u64(ENV_MAX_DIMENSION).map(|d| d as u32),
    };
    cap.wants_cap().then_some(cap)
}

/// What [`apply_image_cap`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapResult {
    /// Final path (extension may have changed PNG → JPG).
    pub final_path: PathBuf,
    /// Final size in bytes.
    pub final_bytes: u64,
    /// Final width.
    pub final_width: u32,
    /// Final height.
    pub final_height: u32,
    /// `"PNG"` or `"JPEG"`.
    pub format: &'static str,
    /// JPEG quality chosen, if any.
    pub quality: Option<u8>,
    /// True iff the constraint was met (false = best effort).
    pub capped: bool,
}

fn resize_to_long_edge(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    if max_dim == 0 || long <= max_dim {
        return img.clone();
    }
    let ratio = f64::from(max_dim) / f64::from(long);
    let nw = ((f64::from(w) * ratio) as u32).max(1);
    let nh = ((f64::from(h) * ratio) as u32).max(1);
    img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| Error::Image(format!("jpeg encode: {e}")))?;
    Ok(buf)
}

/// First quality in the ladder whose encoding fits `max_bytes`; the last
/// (smallest) encoding is returned even when nothing fits.
fn quality_search(img: &DynamicImage, max_bytes: u64) -> Result<(Vec<u8>, u8, bool)> {
    let mut last = Vec::new();
    for q in QUALITY_STEPS {
        last = encode_jpeg(img, q)?;
        if last.len() as u64 <= max_bytes {
            return Ok((last, q, true));
        }
    }
    Ok((last, QUALITY_STEPS[4], false))
}

/// Resize / recompress the image at `path` in place to fit `cap`. May
/// rename PNG → JPG when `max_bytes` is set.
pub fn apply_image_cap(
    path: &Path,
    cap: &ImageCap,
    reserve_bytes_for_metadata: bool,
) -> Result<CapResult> {
    let bytes = std::fs::read(path)?;
    let img = image::load_from_memory(&bytes).map_err(|e| Error::Image(format!("decode: {e}")))?;
    let is_png = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !cap.wants_cap() {
        return Ok(CapResult {
            final_path: path.to_path_buf(),
            final_bytes: bytes.len() as u64,
            final_width: img.width(),
            final_height: img.height(),
            format: if is_png { "PNG" } else { "JPEG" },
            quality: None,
            capped: false,
        });
    }
    let mut img = match cap.max_dimension {
        Some(d) if d > 0 => resize_to_long_edge(&img, d),
        _ => img,
    };
    if let Some(max_bytes) = cap.max_bytes.filter(|b| *b > 0) {
        let effective = if reserve_bytes_for_metadata {
            max_bytes.saturating_sub(METADATA_OVERHEAD_BUDGET).max(1)
        } else {
            max_bytes
        };
        let (mut data, mut quality, mut capped) = quality_search(&img, effective)?;
        if !capped {
            let (hw, hh) = (img.width() / 2, img.height() / 2);
            if hw >= MIN_DIMENSION_AFTER_HALVING && hh >= MIN_DIMENSION_AFTER_HALVING {
                img = img.resize_exact(hw, hh, image::imageops::FilterType::Lanczos3);
                (data, quality, capped) = quality_search(&img, effective)?;
            }
        }
        let jpg_path = path.with_extension("jpg");
        std::fs::write(&jpg_path, &data)?;
        if jpg_path != path && path.exists() {
            let _ = std::fs::remove_file(path);
        }
        return Ok(CapResult {
            final_path: jpg_path,
            final_bytes: data.len() as u64,
            final_width: img.width(),
            final_height: img.height(),
            format: "JPEG",
            quality: Some(quality),
            capped,
        });
    }
    // max_dimension only: stay PNG.
    let rgba = img.to_rgba8();
    geometry::write_png(path, &rgba)?;
    Ok(CapResult {
        final_path: path.to_path_buf(),
        final_bytes: std::fs::metadata(path)?.len(),
        final_width: img.width(),
        final_height: img.height(),
        format: "PNG",
        quality: None,
        capped: true,
    })
}

// ---------------------------------------------------------------------------
// Metadata: PNG tEXt (geometry) / JPEG EXIF UserComment
// ---------------------------------------------------------------------------

/// Build an APP1 EXIF segment (little-endian TIFF, one IFD0 entry pointing
/// at an Exif IFD with a single `UserComment`) carrying `payload`.
fn build_exif_app1(payload: &[u8]) -> Vec<u8> {
    let comment: Vec<u8> = EXIF_USER_COMMENT_PREFIX
        .iter()
        .chain(payload.iter())
        .copied()
        .collect();
    let mut tiff: Vec<u8> = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
                                                 // IFD0: 1 entry — ExifIFDPointer (0x8769) LONG count 1 → offset.
    let ifd0_len = 2 + 12 + 4;
    let exif_ifd_off = 8 + ifd0_len;
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x8769u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&(exif_ifd_off as u32).to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
                                                 // Exif IFD: 1 entry — UserComment (0x9286) UNDEFINED count n → offset.
    let data_off = exif_ifd_off + 2 + 12 + 4;
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x9286u16.to_le_bytes());
    tiff.extend_from_slice(&7u16.to_le_bytes());
    tiff.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    if comment.len() <= 4 {
        let mut v = [0u8; 4];
        v[..comment.len()].copy_from_slice(&comment);
        tiff.extend_from_slice(&v);
    } else {
        tiff.extend_from_slice(&(data_off as u32).to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    if comment.len() > 4 {
        tiff.extend_from_slice(&comment);
    }
    let mut seg = Vec::new();
    seg.extend_from_slice(&[0xFF, 0xE1]);
    let len = (tiff.len() + 6 + 2) as u16;
    seg.extend_from_slice(&len.to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

/// Strip any existing APP1 Exif segment and insert ours right after SOI.
fn jpeg_with_exif(jpeg: &[u8], app1: &[u8]) -> Result<Vec<u8>> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return Err(Error::Image("not a JPEG".to_string()));
    }
    let mut out = Vec::with_capacity(jpeg.len() + app1.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(app1);
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let marker = jpeg[i + 1];
        if marker == 0xDA {
            break; // start of scan — copy the rest verbatim
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let end = (i + 2 + len).min(jpeg.len());
        let is_exif = marker == 0xE1 && jpeg[i + 4..end].starts_with(b"Exif\0\0");
        if !is_exif {
            out.extend_from_slice(&jpeg[i..end]);
        }
        i = end;
    }
    out.extend_from_slice(&jpeg[i..]);
    Ok(out)
}

/// Read the `UserComment` payload out of a JPEG's EXIF, if present.
fn jpeg_exif_comment(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let marker = jpeg[i + 1];
        if marker == 0xDA {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let end = (i + 2 + len).min(jpeg.len());
        if marker == 0xE1 && jpeg[i + 4..end].starts_with(b"Exif\0\0") {
            return parse_tiff_user_comment(&jpeg[i + 10..end]);
        }
        i = end;
    }
    None
}

fn parse_tiff_user_comment(t: &[u8]) -> Option<Vec<u8>> {
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b = [*t.get(o)?, *t.get(o + 1)?];
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = [*t.get(o)?, *t.get(o + 1)?, *t.get(o + 2)?, *t.get(o + 3)?];
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let find_in_ifd = |ifd: usize, tag: u16| -> Option<(u32, u32, usize)> {
        let n = u16_at(ifd)? as usize;
        (0..n).find_map(|k| {
            let e = ifd + 2 + k * 12;
            (u16_at(e)? == tag).then(|| Some((u32_at(e + 4)?, u32_at(e + 8)?, e + 8)))?
        })
    };
    let ifd0 = u32_at(4)? as usize;
    let (_, exif_off, _) = find_in_ifd(ifd0, 0x8769)?;
    let (count, value, value_pos) = find_in_ifd(exif_off as usize, 0x9286)?;
    let count = count as usize;
    let data = if count <= 4 {
        t.get(value_pos..value_pos + count)?
    } else {
        t.get(value as usize..value as usize + count)?
    };
    data.strip_prefix(EXIF_USER_COMMENT_PREFIX)
        .map(<[u8]>::to_vec)
}

/// Embed `meta` into the image at `path` — PNG `tEXt` or JPEG EXIF
/// `UserComment`, by extension. Other formats are a no-op.
pub fn write_metadata(path: &Path, meta: &ScreenshotMeta) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => {
            let bytes = std::fs::read(path)?;
            let img = geometry::decode_png(&bytes)?;
            geometry::write_png_with_metadata(path, &img, meta)
        }
        "jpg" | "jpeg" => {
            let bytes = std::fs::read(path)?;
            let payload = serde_json::to_vec(meta)?;
            let out = jpeg_with_exif(&bytes, &build_exif_app1(&payload))?;
            std::fs::write(path, out)?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Read the metadata back from a PNG or JPEG. `Ok(None)` when absent.
pub fn read_metadata(path: &Path) -> Result<Option<ScreenshotMeta>> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => geometry::read_png_metadata(path),
        "jpg" | "jpeg" => {
            let bytes = std::fs::read(path)?;
            Ok(jpeg_exif_comment(&bytes)
                .and_then(|c| serde_json::from_slice::<ScreenshotMeta>(&c).ok()))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DynamicImage {
        let mut img = image::RgbaImage::new(400, 300);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn exif_roundtrip() {
        let jpg = encode_jpeg(&sample(), 80).unwrap();
        let meta = ScreenshotMeta {
            scale_factor: 1.25,
            viewport_width: 1600,
            viewport_height: 950,
            image_width: 1280,
            image_height: 760,
            device_pixel_ratio: 1.0,
        };
        let payload = serde_json::to_vec(&meta).unwrap();
        let with = jpeg_with_exif(&jpg, &build_exif_app1(&payload)).unwrap();
        let back = jpeg_exif_comment(&with).unwrap();
        assert_eq!(
            serde_json::from_slice::<ScreenshotMeta>(&back).unwrap(),
            meta
        );
        // Re-embedding replaces rather than duplicates.
        let again = jpeg_with_exif(&with, &build_exif_app1(b"{}")).unwrap();
        assert_eq!(jpeg_exif_comment(&again).unwrap(), b"{}");
        assert!(image::load_from_memory(&again).is_ok());
    }

    #[test]
    fn cap_by_bytes_switches_to_jpeg_and_fits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.png");
        geometry::write_png(&p, &sample().to_rgba8()).unwrap();
        let r = apply_image_cap(
            &p,
            &ImageCap {
                max_bytes: Some(20_000),
                max_dimension: Some(200),
            },
            false,
        )
        .unwrap();
        assert_eq!(r.format, "JPEG");
        assert!(r.capped);
        assert!(r.final_bytes <= 20_000);
        assert_eq!(r.final_width, 200);
        assert_eq!(r.final_path.extension().unwrap(), "jpg");
        assert!(!p.exists());
        let meta = ScreenshotMeta {
            scale_factor: 2.0,
            viewport_width: 400,
            viewport_height: 300,
            image_width: 200,
            image_height: 150,
            device_pixel_ratio: 1.0,
        };
        write_metadata(&r.final_path, &meta).unwrap();
        assert_eq!(read_metadata(&r.final_path).unwrap(), Some(meta));
    }

    #[test]
    fn cap_by_dimension_only_stays_png() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.png");
        geometry::write_png(&p, &sample().to_rgba8()).unwrap();
        let r = apply_image_cap(
            &p,
            &ImageCap {
                max_bytes: None,
                max_dimension: Some(100),
            },
            false,
        )
        .unwrap();
        assert_eq!(r.format, "PNG");
        assert_eq!((r.final_width, r.final_height), (100, 75));
        assert_eq!(r.final_path, p);
    }

    #[test]
    fn resolve_precedence() {
        assert!(
            resolve_cap(Some(ImageCap::default())).is_none()
                || std::env::var(ENV_MAX_BYTES).is_ok()
        );
        let c = resolve_cap(Some(ImageCap {
            max_bytes: Some(5),
            max_dimension: None,
        }))
        .unwrap();
        assert_eq!(c.max_bytes, Some(5));
    }
}
