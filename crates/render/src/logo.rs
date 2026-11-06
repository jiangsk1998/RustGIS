//! The HuaGIS logo, the image `tmp/favicon-light.svg` wraps: 773×646, transparent background.
//! The same image is published as SVG; embedding the PNG keeps the app free of runtime file
//! lookups and lets `apps/rustgis/build.rs` draw the Windows icon with no image dependency.

use tiny_skia::{Pixmap, PixmapPaint, Transform};

const LOGO: &[u8] = include_bytes!("../assets/huagis-logo.png");

/// The logo as a premultiplied RGBA pixmap of `size`×`size` pixels (16..=1024), fitted into
/// the square canvas keeping its aspect ratio.
pub fn pixmap(size: u32) -> Option<Pixmap> {
    let size = size.clamp(16, 1024);
    let src = Pixmap::decode_png(LOGO).ok()?;
    let k = f32::min(size as f32 / src.width() as f32, size as f32 / src.height() as f32);
    let x = (size as f32 - src.width() as f32 * k) / 2.0;
    let y = (size as f32 - src.height() as f32 * k) / 2.0;
    let mut pm = Pixmap::new(size, size)?;
    pm.draw_pixmap(x as i32, y as i32, src.as_ref(), &PixmapPaint::default(), Transform::from_scale(k, k), None);
    Some(pm)
}

/// The logo as straight (unpremultiplied) RGBA bytes, for window icons.
pub fn rgba(size: u32) -> Option<(u32, Vec<u8>)> {
    let pm = pixmap(size)?;
    let mut out = Vec::with_capacity(pm.data().len());
    for p in pm.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((pm.width(), out))
}

/// The logo as an ICO file holding PNG images from 16 to 256 px (Windows Vista and later read
/// these), for the exe icon and the Windows installers.
pub fn ico() -> Option<Vec<u8>> {
    const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for size in SIZES {
        images.push((size, pixmap(size)?.encode_png().ok()?));
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&u16::try_from(images.len()).ok()?.to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        // A dimension of 0 means 256.
        let dim = u8::try_from(*size).unwrap_or(0);
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&u32::try_from(png.len()).ok()?.to_le_bytes());
        ico.extend_from_slice(&u32::try_from(offset).ok()?.to_le_bytes());
        offset += png.len();
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }
    Some(ico)
}

/// A 24-bit BMP of `width`×`height` white pixels with the logo (`size` px) drawn at `x`, `y`,
/// for installer wizard images. Sizes are clamped to 1..=2048.
pub fn bmp(size: u32, width: u32, height: u32, x: i32, y: i32) -> Option<Vec<u8>> {
    let (w, h) = (width.clamp(1, 2048), height.clamp(1, 2048));
    let (side, px) = rgba(size)?;
    let row = (w as usize * 3).div_ceil(4) * 4;
    let len = 54 + row * h as usize;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32::try_from(row * h as usize).ok()?.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    // Rows run bottom to top, pixels are BGR, each row padded to four bytes.
    for r in (0..h as i64).rev() {
        let start = out.len();
        for c in 0..w as i64 {
            let (lx, ly) = (c - i64::from(x), r - i64::from(y));
            let mut bgr = [255u8; 3];
            if (0..i64::from(side)).contains(&lx) && (0..i64::from(side)).contains(&ly) {
                let i = usize::try_from((ly * i64::from(side) + lx) * 4).ok()?;
                if let Some(p) = px.get(i..i + 4) {
                    let a = u32::from(p[3]);
                    let over = |v: u8| ((u32::from(v) * a + 255 * (255 - a)) / 255) as u8;
                    bgr = [over(p[2]), over(p[1]), over(p[0])];
                }
            }
            out.extend_from_slice(&bgr);
        }
        out.resize(start + row, 0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn draws_at_every_size() {
        for s in [0, 16, 32, 256, 5000] {
            let (w, px) = super::rgba(s).unwrap();
            assert_eq!(px.len() as u32, w * w * 4);
            assert!(px.chunks(4).any(|c| c[3] == 255), "logo has opaque pixels");
        }
    }

    #[test]
    fn bmp_has_padded_rows_and_the_logo() {
        let bmp = super::bmp(32, 50, 40, 9, 4).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 152 * 40);
        assert!(bmp[54..].chunks(3).any(|c| c != [255, 255, 255]), "logo pixels");
        // A logo placed off the canvas leaves it white.
        let blank = super::bmp(32, 10, 10, 500, -500).unwrap();
        assert!(blank[54..].chunks(32).all(|r| r[..30].iter().all(|&b| b == 255)));
    }

    #[test]
    fn ico_lists_seven_png_images() {
        let ico = super::ico().unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 7, 0]);
        // The first image starts right after the directory, with the PNG signature.
        let offset = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(offset, 6 + 16 * 7);
        assert_eq!(&ico[offset..offset + 4], b"\x89PNG");
    }
}
