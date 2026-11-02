//! Elevation tiles (`raster-dem` sources) drawn as `color-relief` and `hillshade` layers.
//!
//! The heights under the pixmap are read once per source into a [`Grid`] a pixel larger all
//! round than the pixmap, from whichever tiles are there (stand-ins first, the tiles themselves
//! over them), so neither layer shows where tiles meet. Colour relief colours each pixel by its
//! height. Hillshade lights the surface from its slope across each pixel's neighbours, by Horn's
//! method (B. K. P. Horn, "Hill shading and the reflectance map", Proc. IEEE 69(1), 1981): the
//! slopes facing the light take the highlight colour and those facing away the shadow colour, in
//! proportion to how much more or less light they catch than flat ground does, so flat ground is
//! left as it is.

use tiny_skia::{ColorU8, Pixmap, PixmapPaint, PremultipliedColorU8, Transform};

use crate::cache::Payload;
use crate::draw::{Frame, Item};
use crate::style::{DemEncoding, ElevationRamp, Hillshade, HillshadeMethod, LayerKind, Rgba, StyleLayer};

/// Web Mercator's sphere, metres.
const EARTH_RADIUS: f64 = 6_378_137.0;

/// Ground metres a pixel across at which slopes are drawn as steep as they are; at coarser
/// scales they are steepened (see [`steepening`]).
const TRUE_SLOPE_PX: f64 = 30.0;

/// The most slopes are steepened by, at the coarsest scales.
const MAX_STEEPENING: f64 = 32.0;

/// Heights under a `w × h` pixmap and a pixel past each edge; NaN where there is none.
pub(crate) struct Grid {
    w: usize,
    h: usize,
    cells: Vec<f32>,
    /// Web Mercator y of the pixmap's top edge, and metres per pixel.
    top: f64,
    m_per_px: f64,
}

impl Grid {
    /// Reads the heights of `items` (a `raster-dem` source's tiles, as [`crate::draw`] gathers
    /// them) at the centres of the pixels of a `w × h` pixmap at `frame`, from the nearest tile
    /// pixel or blended from the four around it.
    pub(crate) fn read(items: &[Item], encoding: DemEncoding, frame: &Frame, (w, h): (u32, u32), nearest: bool) -> Grid {
        let (w, h) = (w as usize, h as usize);
        let (gw, gh) = (w + 2, h + 2);
        let mut cells = vec![f32::NAN; gw * gh];
        let m = frame.m_per_px;
        // The cell at column `i` is centred half a pixel into pixel `i - 1`.
        let x_of = |i: usize| frame.left + (i as f64 - 0.5) * m;
        let y_of = |j: usize| frame.top - (j as f64 - 0.5) * m;
        // The first cell whose centre is at or past a coordinate (as a float, cast saturating).
        let col = |x: f64| ((x - frame.left) / m + 0.5).ceil().max(0.0);
        let row = |y: f64| ((frame.top - y) / m + 0.5).ceil().max(0.0);
        for it in items {
            let Payload::Image(pm) = &*it.payload else { continue };
            let (pw, ph) = (pm.width() as usize, pm.height() as usize);
            if pw == 0 || ph == 0 || it.at.width() <= 0.0 || it.at.height() <= 0.0 {
                continue;
            }
            let (sx, sy) = (pw as f64 / it.at.width(), ph as f64 / it.at.height());
            // A tile pixel's height; one that isn't opaque has none. One a pixel past the tile's
            // edge is the next tile's, so a blend near the edge runs on into it.
            let texel = |u: isize, v: isize| match (usize::try_from(u), usize::try_from(v)) {
                (Ok(u), Ok(v)) if u < pw && v < ph => height_of(pm, u, v, encoding),
                _ => {
                    let (x, y) = (it.at.min_x + (u as f64 + 0.5) / sx, it.at.max_y - (v as f64 + 0.5) / sy);
                    items.iter().rev().filter(|o| !std::ptr::eq(*o, it)).find_map(|o| height_at(o, x, y, encoding)).unwrap_or(f32::NAN)
                }
            };
            let (i0, i1) = (col(it.clip.min_x) as usize, (col(it.clip.max_x) as usize).min(gw));
            let (j0, j1) = (row(it.clip.max_y) as usize, (row(it.clip.min_y) as usize).min(gh));
            for j in j0..j1 {
                let v = (it.at.max_y - y_of(j)) * sy - 0.5;
                for i in i0..i1 {
                    let u = (x_of(i) - it.at.min_x) * sx - 0.5;
                    let height = sample(&texel, u, v, pw, ph, nearest);
                    if let Some(c) = cells.get_mut(j * gw + i) {
                        *c = height;
                    }
                }
            }
        }
        Grid { w, h, cells, top: frame.top, m_per_px: m }
    }

    /// The height at pixel (`x`, `y`) of the pixmap; `-1` and `w`/`h` are the cells past its
    /// edges.
    fn at(&self, x: isize, y: isize) -> f32 {
        let (i, j) = (x + 1, y + 1);
        if i < 0 || j < 0 || i as usize >= self.w + 2 || j as usize >= self.h + 2 {
            return f32::NAN;
        }
        self.cells.get(j as usize * (self.w + 2) + i as usize).copied().unwrap_or(f32::NAN)
    }
}

/// The height of a tile's pixel (`u`, `v`); one that isn't opaque has none.
fn height_of(pm: &Pixmap, u: usize, v: usize, encoding: DemEncoding) -> f32 {
    let w = pm.width() as usize;
    pm.pixels().get(v * w + u).filter(|c| c.alpha() == 255).map_or(f32::NAN, |c| encoding.height([c.red(), c.green(), c.blue()]))
}

/// The height of `it`'s pixel nearest a point it stands in for, if it does.
fn height_at(it: &Item, x: f64, y: f64, encoding: DemEncoding) -> Option<f32> {
    let Payload::Image(pm) = &*it.payload else { return None };
    if !(it.clip.min_x <= x && x < it.clip.max_x && it.clip.min_y < y && y <= it.clip.max_y) {
        return None;
    }
    let (pw, ph) = (pm.width(), pm.height());
    let u = ((x - it.at.min_x) / it.at.width() * f64::from(pw)).floor().clamp(0.0, f64::from(pw.max(1) - 1));
    let v = ((it.at.max_y - y) / it.at.height() * f64::from(ph)).floor().clamp(0.0, f64::from(ph.max(1) - 1));
    Some(height_of(pm, u as usize, v as usize, encoding))
}

/// The height at tile pixel coordinates (`u`, `v`) (pixel centres at whole numbers, the tile's
/// own from 0 to `pw - 1` and `ph - 1`), from the nearest pixel or blended from the four around
/// it, which may be past the tile's edge; a blend over a pixel with no height takes the tile's
/// own nearest instead.
fn sample(texel: &impl Fn(isize, isize) -> f32, u: f64, v: f64, pw: usize, ph: usize, nearest: bool) -> f32 {
    let clamp = |p: f64, n: usize| p.clamp(0.0, (n - 1) as f64);
    let near = || texel(clamp(u, pw).round() as isize, clamp(v, ph).round() as isize);
    if nearest {
        return near();
    }
    // Within half a pixel past the tile's own: the blend's far pixels are its neighbour's.
    let (u, v) = (u.clamp(-0.5, pw as f64 - 0.5), v.clamp(-0.5, ph as f64 - 0.5));
    let (u0, v0) = (u.floor() as isize, v.floor() as isize);
    let (u1, v1) = (u0 + 1, v0 + 1);
    let (fu, fv) = ((u - u0 as f64) as f32, (v - v0 as f64) as f32);
    let (a, b, c, d) = (texel(u0, v0), texel(u1, v0), texel(u0, v1), texel(u1, v1));
    if a.is_nan() || b.is_nan() || c.is_nan() || d.is_nan() {
        return near();
    }
    let top = a + (b - a) * fu;
    let bottom = c + (d - c) * fu;
    top + (bottom - top) * fv
}

/// Draws a `color-relief` or `hillshade` `layer` from `grid` into `target` (the pixmap the grid
/// was read for).
pub(crate) fn draw(target: &mut Pixmap, layer: &StyleLayer, grid: &Grid, zoom: f32) {
    let (picture, opacity) = match layer.kind {
        LayerKind::ColorRelief => match layer.color_relief_ramp(zoom) {
            Some(ramp) => (color_relief(grid, &ramp), layer.color_relief_opacity(zoom)),
            None => {
                log::debug!("color-relief layer {}: its color-relief-color isn't a ramp on [\"elevation\"]", layer.id);
                return;
            }
        },
        LayerKind::Hillshade => (hillshade(grid, &layer.hillshade(zoom)), 1.0),
        _ => return,
    };
    if let Some(p) = picture
        && opacity > 0.0
    {
        target.draw_pixmap(0, 0, p.as_ref(), &PixmapPaint { opacity, ..PixmapPaint::default() }, Transform::identity(), None);
    }
}

/// Each pixel in the colour of its height.
fn color_relief(grid: &Grid, ramp: &ElevationRamp) -> Option<Pixmap> {
    let mut pm = Pixmap::new(grid.w as u32, grid.h as u32)?;
    let w = grid.w;
    for (n, px) in pm.pixels_mut().iter_mut().enumerate() {
        let height = grid.at((n % w) as isize, (n / w) as isize);
        if height.is_finite() {
            *px = premultiplied(ramp.color(f64::from(height)), 1.0);
        }
    }
    Some(pm)
}

/// How much slopes are steepened where a pixel is `ground` metres across: not at all at
/// [`TRUE_SLOPE_PX`] or finer, and more the coarser the scale, so that relief a small-scale map
/// smooths into gentle slopes still shows.
fn steepening(ground: f64) -> f64 {
    (ground / TRUE_SLOPE_PX).sqrt().clamp(1.0, MAX_STEEPENING)
}

/// A light: towards it (east, north, up), its shading on flat ground, and its colours.
struct Light {
    to: [f64; 3],
    flat: f64,
    shadow: Rgba,
    highlight: Rgba,
}

/// The layer's lights: one, or for the multidirectional method one from each direction.
fn lights(hs: &Hillshade) -> Vec<Light> {
    let n = if hs.method == HillshadeMethod::Multidirectional { hs.directions.len() } else { 1 };
    // The value at `i`, or the list's last when it is shorter.
    fn nth<T: Copy>(list: &[T], i: usize, default: T) -> T {
        list.get(i).or(list.last()).copied().unwrap_or(default)
    }
    (0..n)
        .map(|i| {
            let azimuth = f64::from(nth(&hs.directions, i, 335.0)).to_radians();
            let altitude = f64::from(nth(&hs.altitudes, i, 45.0)).clamp(0.0, 90.0).to_radians();
            Light {
                to: [azimuth.sin() * altitude.cos(), azimuth.cos() * altitude.cos(), altitude.sin()],
                flat: altitude.sin(),
                shadow: nth(&hs.shadows, i, [0, 0, 0, 255]),
                highlight: nth(&hs.highlights, i, [255, 255, 255, 255]),
            }
        })
        .collect()
}

/// The shading of each pixel's slope.
fn hillshade(grid: &Grid, hs: &Hillshade) -> Option<Pixmap> {
    let mut pm = Pixmap::new(grid.w as u32, grid.h as u32)?;
    let lights = lights(hs);
    let exaggeration = f64::from(hs.exaggeration);
    // Shading at its strongest is opaque at the greatest exaggeration; several lights share it.
    let strength = 2.0 * exaggeration / lights.len().max(1) as f64;
    let w = grid.w;
    let pixels = pm.pixels_mut();
    for y in 0..grid.h {
        // Web Mercator stretches the ground by 1 / cos(latitude) = cosh(y / R).
        let mercator_y = grid.top - (y as f64 + 0.5) * grid.m_per_px;
        let ground = grid.m_per_px / (mercator_y / EARTH_RADIUS).cosh();
        if !(ground.is_finite() && ground > 0.0) {
            continue;
        }
        let scale = steepening(ground) / (8.0 * ground);
        for x in 0..w {
            let (xi, yi) = (x as isize, y as isize);
            let centre = grid.at(xi, yi);
            if !centre.is_finite() {
                continue;
            }
            // A neighbour with no height is taken as level with the centre.
            let z = |dx: isize, dy: isize| {
                let v = grid.at(xi + dx, yi + dy);
                f64::from(if v.is_finite() { v } else { centre })
            };
            let (a, b, c) = (z(-1, -1), z(0, -1), z(1, -1));
            let (d, f) = (z(-1, 0), z(1, 0));
            let (g, h, i) = (z(-1, 1), z(0, 1), z(1, 1));
            // Rise to the east and to the north (rows run south).
            let east = ((c + 2.0 * f + i) - (a + 2.0 * d + g)) * scale;
            let north = ((a + 2.0 * b + c) - (g + 2.0 * h + i)) * scale;
            let gradient = (east * east + north * north).sqrt();
            let len = (1.0 + gradient * gradient).sqrt();
            // The surface's normal, and the sine and cosine of its slope.
            let normal = [-east / len, -north / len, 1.0 / len];
            let (slope_sin, slope_cos) = (gradient / len, 1.0 / len);
            let mut out = [0.0f32; 4];
            if hs.method == HillshadeMethod::Standard {
                over(&mut out, hs.accent, exaggeration * (1.0 - slope_cos));
            }
            for l in &lights {
                let shade = match hs.method {
                    // By how steep the slope is and how far it turns from the light, whatever the
                    // light's altitude: gentle slopes stay light.
                    HillshadeMethod::Igor => {
                        let across = (l.to[0] * l.to[0] + l.to[1] * l.to[1]).sqrt();
                        let facing =
                            if gradient > 0.0 && across > 0.0 { (-east * l.to[0] - north * l.to[1]) / (gradient * across) } else { 0.0 };
                        slope_sin * facing
                    }
                    _ => {
                        let lit = normal[0] * l.to[0] + normal[1] * l.to[1] + normal[2] * l.to[2];
                        let shade = lit - l.flat;
                        // Darker still the steeper the slope.
                        if hs.method == HillshadeMethod::Combined { shade - 0.5 * (1.0 - slope_cos) } else { shade }
                    }
                };
                let color = if shade > 0.0 { l.highlight } else { l.shadow };
                over(&mut out, color, (shade.abs() * strength).min(1.0));
            }
            if let Some(px) = pixels.get_mut(y * w + x) {
                *px = to_pixel(out);
            }
        }
    }
    Some(pm)
}

/// Lays `color` at `alpha` (times its own) over premultiplied `out`.
fn over(out: &mut [f32; 4], [r, g, b, a]: Rgba, alpha: f64) {
    let a = f32::from(a) / 255.0 * alpha.clamp(0.0, 1.0) as f32;
    if a <= 0.0 {
        return;
    }
    let keep = 1.0 - a;
    for (o, c) in out.iter_mut().zip([r, g, b]) {
        *o = f32::from(c) / 255.0 * a + *o * keep;
    }
    out[3] = a + out[3] * keep;
}

fn to_pixel([r, g, b, a]: [f32; 4]) -> PremultipliedColorU8 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = u(a);
    // Premultiplied channels never exceed alpha.
    PremultipliedColorU8::from_rgba(u(r).min(a), u(g).min(a), u(b).min(a), a).unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

fn premultiplied([r, g, b, a]: Rgba, opacity: f32) -> PremultipliedColorU8 {
    let a = (f32::from(a) * opacity.clamp(0.0, 1.0)).round() as u8;
    ColorU8::from_rgba(r, g, b, a).premultiply()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Expr;
    use rustgis_geom::Extent;
    use std::sync::Arc;

    /// A terrarium tile `side` pixels a side over `at`, its heights from `height(u, v)`.
    fn tile(side: u32, at: Extent, height: impl Fn(u32, u32) -> f64) -> Item {
        let mut pm = Pixmap::new(side, side).unwrap();
        for (n, px) in pm.pixels_mut().iter_mut().enumerate() {
            let (u, v) = (n as u32 % side, n as u32 / side);
            let t = height(u, v) + 32768.0;
            let (r, g, b) = ((t / 256.0).floor(), t.rem_euclid(256.0).floor(), (t.fract() * 256.0).round());
            *px = ColorU8::from_rgba(r as u8, g as u8, b as u8, 255).premultiply();
        }
        Item { payload: Arc::new(Payload::Image(pm)), at, z: 0, clip: at }
    }

    /// A 64-pixel view of a 64-pixel tile over the same square, near the equator.
    fn frame(at: &Extent) -> Frame {
        Frame { left: at.min_x, top: at.max_y, m_per_px: at.width() / 64.0, ppp: 1.0 }
    }

    #[test]
    fn heights_decode_by_encoding() {
        assert_eq!(DemEncoding::Terrarium.height([128, 0, 0]), 0.0);
        assert_eq!(DemEncoding::Terrarium.height([127, 255, 128]), -0.5);
        assert_eq!(DemEncoding::Mapbox.height([1, 134, 160]), 0.0);
        let custom = DemEncoding::Custom { red: 1.0, green: 2.0, blue: 3.0, base: 4.0 };
        assert_eq!(custom.height([1, 1, 1]), 2.0);
    }

    #[test]
    fn grid_reads_the_tile_under_each_pixel() {
        let at = Extent::new(0.0, 0.0, 6400.0, 6400.0);
        let it = tile(64, at, |u, v| f64::from(u) * 10.0 - f64::from(v));
        let g = Grid::read(&[it], DemEncoding::Terrarium, &frame(&at), (64, 64), true);
        assert_eq!(g.at(0, 0), 0.0);
        assert_eq!(g.at(5, 3), 47.0);
        // The cells past the pixmap's edges are past the tile's too.
        assert!(g.at(-1, 0).is_nan() && g.at(64, 0).is_nan());
        // Blended: halfway between pixels is halfway between heights.
        let half = Frame { left: at.min_x + 50.0, ..frame(&at) };
        let g = Grid::read(&[tile(64, at, |u, _| f64::from(u) * 10.0)], DemEncoding::Terrarium, &half, (8, 8), false);
        assert!((g.at(2, 2) - 25.0).abs() < 1e-3, "{}", g.at(2, 2));
    }

    #[test]
    fn grid_leaves_cells_no_tile_covers_empty() {
        let at = Extent::new(0.0, 0.0, 3200.0, 6400.0);
        let f = Frame { left: 0.0, top: 6400.0, m_per_px: 100.0, ppp: 1.0 };
        let g = Grid::read(&[tile(32, at, |_, _| 5.0)], DemEncoding::Terrarium, &f, (64, 64), true);
        assert_eq!(g.at(10, 10), 5.0);
        assert!(g.at(40, 10).is_nan());
    }

    #[test]
    fn color_relief_colours_by_height_and_skips_no_data() {
        let at = Extent::new(0.0, 0.0, 3200.0, 6400.0);
        let f = Frame { left: 0.0, top: 6400.0, m_per_px: 100.0, ppp: 1.0 };
        let g = Grid::read(&[tile(32, at, |u, _| if u < 16 { -100.0 } else { 100.0 })], DemEncoding::Terrarium, &f, (64, 64), true);
        let ramp = Expr::property(&serde_json::json!(["step", ["elevation"], "#0000ff", 0, "#00ff00"])).elevation_ramp(10.0).unwrap();
        let pm = color_relief(&g, &ramp).unwrap();
        let rgba = |x: u32, y: u32| {
            let p = pm.pixel(x, y).unwrap();
            [p.red(), p.green(), p.blue(), p.alpha()]
        };
        assert_eq!(rgba(5, 5), [0, 0, 255, 255]);
        assert_eq!(rgba(20, 5), [0, 255, 0, 255]);
        assert_eq!(rgba(50, 5), [0, 0, 0, 0]);
    }

    /// The hillshade of a plane rising to the east, lit from `direction`.
    fn shaded(method: HillshadeMethod, direction: f32) -> Pixmap {
        let at = Extent::new(0.0, 0.0, 6400.0, 6400.0);
        let g = Grid::read(&[tile(64, at, |u, _| f64::from(u) * 50.0)], DemEncoding::Terrarium, &frame(&at), (64, 64), false);
        let hs = Hillshade {
            method,
            exaggeration: 0.5,
            directions: vec![direction],
            altitudes: vec![45.0],
            shadows: vec![[0, 0, 0, 255]],
            highlights: vec![[255, 255, 255, 255]],
            accent: [0, 0, 0, 0],
        };
        hillshade(&g, &hs).unwrap()
    }

    #[test]
    fn hillshade_lights_slopes_facing_the_light() {
        for method in [HillshadeMethod::Standard, HillshadeMethod::Igor, HillshadeMethod::Basic] {
            // Rising to the east, the slope faces west.
            let west = shaded(method, 270.0);
            let p = west.pixel(32, 32).unwrap();
            assert!(p.alpha() > 0 && p.red() == p.alpha(), "{method:?}: lit from the west, highlighted: {p:?}");
            let east = shaded(method, 90.0);
            let p = east.pixel(32, 32).unwrap();
            assert!(p.alpha() > 0 && p.red() == 0, "{method:?}: lit from the east, shadowed: {p:?}");
        }
    }

    #[test]
    fn hillshade_runs_on_across_tile_edges() {
        // One even slope rising east over two tiles, the view across the edge between them, half
        // a tile pixel off it, so pixels near the edge blend the two tiles.
        let (west, east) = (Extent::new(0.0, 0.0, 6400.0, 6400.0), Extent::new(6400.0, 0.0, 12800.0, 6400.0));
        let tiles = [tile(64, west, |u, _| f64::from(u) * 50.0), tile(64, east, |u, _| f64::from(u + 64) * 50.0)];
        let f = Frame { left: 3250.0, top: 6400.0, m_per_px: 100.0, ppp: 1.0 };
        let g = Grid::read(&tiles, DemEncoding::Terrarium, &f, (64, 64), false);
        let hs = Hillshade {
            method: HillshadeMethod::Standard,
            exaggeration: 0.5,
            directions: vec![270.0],
            altitudes: vec![45.0],
            shadows: vec![[0, 0, 0, 255]],
            highlights: vec![[255, 255, 255, 255]],
            accent: [0, 0, 0, 0],
        };
        let pm = hillshade(&g, &hs).unwrap();
        // Either side of the edge shades as the middle of a tile does: its neighbours' heights are read.
        let inside = pm.pixel(10, 32).unwrap();
        assert!(inside.alpha() > 0);
        for x in 29..=33 {
            assert_eq!(pm.pixel(x, 32), Some(inside), "column {x}");
        }
        // The heights themselves run on evenly: pixel 31 is centred on the edge, at x = 6400.
        for (x, want) in [(30, 3125.0), (31, 3175.0), (32, 3225.0)] {
            assert!((g.at(x, 32) - want).abs() < 1e-3, "column {x}: {}", g.at(x, 32));
        }
        // Without a neighbour, the edge's own pixel stands in, as before (pixel 31 at x = 6370).
        let f = Frame { left: 3220.0, ..f };
        let g = Grid::read(&tiles[..1], DemEncoding::Terrarium, &f, (64, 64), false);
        assert!((g.at(31, 32) - 3150.0).abs() < 1e-3, "{}", g.at(31, 32));
    }

    #[test]
    fn hillshade_leaves_flat_ground_alone() {
        let at = Extent::new(0.0, 0.0, 6400.0, 6400.0);
        let g = Grid::read(&[tile(64, at, |_, _| -20.0)], DemEncoding::Terrarium, &frame(&at), (64, 64), false);
        let hs = Hillshade {
            method: HillshadeMethod::Standard,
            exaggeration: 1.0,
            directions: vec![335.0],
            altitudes: vec![45.0],
            shadows: vec![[0, 0, 0, 255]],
            highlights: vec![[255, 255, 255, 255]],
            accent: [0, 0, 0, 255],
        };
        assert!(hillshade(&g, &hs).unwrap().pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn coarse_scales_steepen_slopes() {
        assert_eq!(steepening(10.0), 1.0);
        assert!(steepening(3000.0) > 5.0);
        assert_eq!(steepening(1e9), MAX_STEEPENING);
    }
}
