//! Image attributes: what an image binary is (its format and size, from its header, without
//! decoding it) and the small thumbnail the interface shows. Any source's binary column can be
//! offered this way; the original bytes stay in the source and are read when needed.

use base64::Engine;

use crate::{DataError, Result};

/// The longest side of an image attribute's thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 160;
/// The largest image attribute read at once.
pub const MAX_BYTES: usize = 64 << 20;
/// The largest side an image is decoded at (a hostile file's declared size is checked first).
const MAX_SIDE: u32 = 16_384;
/// The most a decode may allocate.
const MAX_ALLOC: u64 = 256 << 20;

/// An image format RustGIS recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
    Tiff,
}

impl PictureFormat {
    pub fn mime(self) -> &'static str {
        match self {
            PictureFormat::Jpeg => "image/jpeg",
            PictureFormat::Png => "image/png",
            PictureFormat::WebP => "image/webp",
            PictureFormat::Gif => "image/gif",
            PictureFormat::Tiff => "image/tiff",
        }
    }

    /// The short name a summary says: `jpeg`, `png`…
    pub fn name(self) -> &'static str {
        match self {
            PictureFormat::Jpeg => "jpeg",
            PictureFormat::Png => "png",
            PictureFormat::WebP => "webp",
            PictureFormat::Gif => "gif",
            PictureFormat::Tiff => "tiff",
        }
    }

    fn image(self) -> image::ImageFormat {
        match self {
            PictureFormat::Jpeg => image::ImageFormat::Jpeg,
            PictureFormat::Png => image::ImageFormat::Png,
            PictureFormat::WebP => image::ImageFormat::WebP,
            PictureFormat::Gif => image::ImageFormat::Gif,
            PictureFormat::Tiff => image::ImageFormat::Tiff,
        }
    }
}

/// The image format of `bytes`, by its magic bytes; `None` for other binary.
pub fn format(bytes: &[u8]) -> Option<PictureFormat> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(PictureFormat::Jpeg);
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(PictureFormat::Png);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(PictureFormat::Gif);
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some(PictureFormat::WebP);
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some(PictureFormat::Tiff);
    }
    None
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(s))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    let s: [u8; 2] = b.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(s))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(s))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(s))
}

/// PNG's IHDR width and height.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(12..16)? == b"IHDR").then(|| Some((be32(b, 16)?, be32(b, 20)?))).flatten()
}

/// GIF's logical screen width and height.
fn gif_size(b: &[u8]) -> Option<(u32, u32)> {
    Some((u32::from(le16(b, 6)?), u32::from(le16(b, 8)?)))
}

/// The size in a JPEG's SOF marker, before its scan starts.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2usize;
    for _ in 0..256 {
        while b.get(at) == Some(&0xFF) && b.get(at.checked_add(1)?) == Some(&0xFF) {
            at = at.checked_add(1)?;
        }
        if b.get(at)? != &0xFF {
            return None;
        }
        let marker = *b.get(at.checked_add(1)?)?;
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            at = at.checked_add(2)?;
            continue;
        }
        let len = usize::from(be16(b, at.checked_add(2)?)?);
        if len < 2 {
            return None;
        }
        // SOF0…SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((be16(b, at.checked_add(7)?)? as u32, be16(b, at.checked_add(5)?)? as u32));
        }
        at = at.checked_add(2)?.checked_add(len)?;
    }
    None
}

/// WebP's canvas width and height, from its VP8X, VP8 or VP8L chunk.
fn webp_size(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        b"VP8 " => {
            if b.get(23..26)? != [0x9D, 0x01, 0x2A] {
                return None;
            }
            Some((u32::from(le16(b, 26)? & 0x3FFF), u32::from(le16(b, 28)? & 0x3FFF)))
        }
        b"VP8L" => {
            if b.get(20) != Some(&0x2F) {
                return None;
            }
            let b1 = *b.get(21)?;
            let b2 = *b.get(22)?;
            let b3 = *b.get(23)?;
            let w = 1 + (u32::from(b1 & 0x3F) << 8) + u32::from(b2);
            let h = 1 + (u32::from(b3 & 0x0F) << 10) + (u32::from(b2 & 0xC0) << 2) + u32::from(b1 >> 6);
            Some((w, h))
        }
        _ => None,
    }
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(3)?)?;
    Some(u32::from(s[0]) | (u32::from(s[1]) << 8) | (u32::from(s[2]) << 16))
}

/// TIFF's IFD0 image width and height.
fn tiff_size(b: &[u8]) -> Option<(u32, u32)> {
    let le = b.get(..4)? == b"II*\0";
    let u16_at = |at: usize| if le { le16(b, at) } else { be16(b, at) };
    let u32_at = |at: usize| if le { le32(b, at) } else { be32(b, at) };
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let n = usize::from(u16_at(ifd)?).min(1000);
    let mut width = None;
    let mut height = None;
    for i in 0..n {
        let e = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        let tag = u16_at(e)?;
        if !matches!(tag, 256 | 257) {
            continue;
        }
        let ty = u16_at(e.checked_add(2)?)?;
        let count = u32_at(e.checked_add(4)?)?;
        if count < 1 {
            continue;
        }
        let v = match ty {
            3 => u32::from(u16_at(e.checked_add(8)?)?),
            4 => u32_at(e.checked_add(8)?)?,
            _ => continue,
        };
        if tag == 256 {
            width = Some(v);
        } else {
            height = Some(v);
        }
    }
    Some((width?, height?))
}

/// The pixel width and height of `bytes` of `format`, read from its header.
pub fn dimensions(bytes: &[u8], format: PictureFormat) -> Option<(u32, u32)> {
    let (w, h) = match format {
        PictureFormat::Jpeg => jpeg_size(bytes)?,
        PictureFormat::Png => png_size(bytes)?,
        PictureFormat::Gif => gif_size(bytes)?,
        PictureFormat::WebP => webp_size(bytes)?,
        PictureFormat::Tiff => tiff_size(bytes)?,
    };
    (w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE).then_some((w, h))
}

/// An image's format, size and byte count, read from its header; `None` when it isn't one.
pub fn meta(bytes: &[u8]) -> Option<(PictureFormat, u32, u32)> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let format = format(bytes)?;
    let (w, h) = dimensions(bytes, format).unwrap_or((0, 0));
    Some((format, w, h))
}

/// A thumbnail of an image, at most [`THUMB_SIZE`] pixels a side, as a `data:image/jpeg;base64,…`
/// address. Decoding is bounded, so a hostile file can't unpack into a huge picture.
pub fn thumbnail(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_BYTES {
        return Err(DataError::Format(format!("the picture is larger than {} MB", MAX_BYTES >> 20)));
    }
    let format = format(bytes).ok_or_else(|| DataError::Format("the value isn't an image".into()))?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format.image());
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| DataError::Format(format!("the picture: {e}")))?.thumbnail(THUMB_SIZE, THUMB_SIZE);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70)
        .encode_image(&img.to_rgb8())
        .map_err(|e| DataError::Format(format!("the picture's thumbnail: {e}")))?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(out)))
}

/// A full image decoded for a viewer, with its sides bounded by `max_side` and its allocation by
/// [`MAX_ALLOC`].
pub fn decode(bytes: &[u8], max_side: u32) -> Result<image::RgbaImage> {
    if bytes.len() > MAX_BYTES {
        return Err(DataError::Format(format!("the picture is larger than {} MB", MAX_BYTES >> 20)));
    }
    let format = format(bytes).ok_or_else(|| DataError::Format("the value isn't an image".into()))?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format.image());
    let mut limits = image::Limits::default();
    let side = max_side.clamp(1, MAX_SIDE);
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    Ok(reader.decode().map_err(|e| DataError::Format(format!("the picture: {e}")))?.to_rgba8())
}

/// An image re-encoded no larger than `max_side` on its longer side, as JPEG bytes. Used when a
/// model or a request refuses the original size.
pub fn shrink(bytes: &[u8], max_side: u32) -> Result<Vec<u8>> {
    let format = format(bytes).ok_or_else(|| DataError::Format("the value isn't an image".into()))?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format.image());
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| DataError::Format(format!("the picture: {e}")))?;
    let max_side = max_side.clamp(1, MAX_SIDE);
    let img = if img.width() > max_side || img.height() > max_side { img.thumbnail(max_side, max_side) } else { img };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&img.to_rgb8())
        .map_err(|e| DataError::Format(format!("the picture: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&vec![0u8; (w * h * 3) as usize], w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn recognizes_formats_and_sizes_from_their_headers() {
        let bytes = png(37, 11);
        assert_eq!(format(&bytes), Some(PictureFormat::Png));
        assert_eq!(dimensions(&bytes, PictureFormat::Png), Some((37, 11)));
        assert_eq!(meta(&bytes).map(|m| (m.0, m.1, m.2)), Some((PictureFormat::Png, 37, 11)));

        let mut gif = b"GIF89a".to_vec();
        gif.extend([5, 0, 9, 0]);
        gif.resize(13, 0);
        assert_eq!(dimensions(&gif, PictureFormat::Gif), Some((5, 9)));

        let mut tiff = b"II*\0".to_vec();
        tiff.extend([8, 0, 0, 0]);
        tiff.extend([2, 0]);
        for (tag, v) in [(256u16, 30u32), (257, 20)] {
            tiff.extend(tag.to_le_bytes());
            tiff.extend([4, 0]);
            tiff.extend(1u32.to_le_bytes());
            tiff.extend(v.to_le_bytes());
        }
        assert_eq!(dimensions(&tiff, PictureFormat::Tiff), Some((30, 20)));

        assert!(format(b"not an image").is_none() && meta(b"not an image").is_none());
        // Every truncation of a real image is refused, never a panic.
        let full = png(8, 8);
        for n in 0..full.len() {
            let _ = meta(&full[..n]);
            let _ = thumbnail(&full[..n]);
        }
    }

    #[test]
    fn thumbnails_are_small_jpeg_addresses() {
        let text = thumbnail(&png(500, 200)).unwrap();
        assert!(text.starts_with("data:image/jpeg;base64,"));
        assert!(text.len() < 40_000);
        assert!(thumbnail(b"junk").is_err());
    }
}
