//! Geometry blobs of a general SQLite table: GeoPackage, SpatiaLite and plain WKB/EWKB.
//!
//! A table may store geometry in any of these forms, or in none at all (an attributes or picture
//! table). This tells them apart and hands back the WKB body [`crate::wkb`] decodes. The bytes are
//! hostile: a malformed blob is `None`, never a panic.

use std::borrow::Cow;

use rustgis_geom::Extent;

use super::gpkg_blob::{is_empty_blob, split_blob as gpkg_split};

/// How a table stores its geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeometryFormat {
    /// A GeoPackage `GP` header and its WKB.
    Gpkg,
    /// A SpatiaLite header (SRID and minimum bounding rectangle) and its WKB.
    SpatiaLite,
    /// Plain WKB, or EWKB with its SRID.
    Wkb,
}

/// The header length of a SpatiaLite blob: the marker, the byte order, the SRID, the MBR and the
/// class marker.
const SPATIALITE_HEADER: usize = 39;

/// The kind of geometry a WKB body holds, as stored (single or multi).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Point,
    MultiPoint,
    Line,
    MultiLine,
    Polygon,
    MultiPolygon,
    Collection,
}

#[cfg(not(target_arch = "wasm32"))]
impl Shape {
    /// The shape of a WKB body, ignoring its Z and M dimensions.
    pub(crate) fn of(body: &[u8]) -> Option<Shape> {
        let little = match body.first()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let ty = read_u32(body.get(1..5)?, little)? & 0x0FFF_FFFF;
        if ty / 1000 > 3 {
            return None;
        }
        Some(match ty % 1000 {
            1 => Shape::Point,
            2 => Shape::Line,
            3 => Shape::Polygon,
            4 => Shape::MultiPoint,
            5 => Shape::MultiLine,
            6 => Shape::MultiPolygon,
            7 => Shape::Collection,
            _ => return None,
        })
    }

    /// Its name, as a project keeps it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Shape::Point => "point",
            Shape::MultiPoint => "multipoint",
            Shape::Line => "line",
            Shape::MultiLine => "multiline",
            Shape::Polygon => "polygon",
            Shape::MultiPolygon => "multipolygon",
            Shape::Collection => "collection",
        }
    }

    /// Whether it holds several parts.
    pub(crate) fn is_multi(self) -> bool {
        matches!(self, Shape::MultiPoint | Shape::MultiLine | Shape::MultiPolygon | Shape::Collection)
    }
}

/// A SpatiaLite blob around a 2D little-endian WKB body: the marker, byte order, SRID and MBR, the
/// class marker, the geometry class the body's type names (Z and M flags kept) and its coordinates.
/// `None` for a body that isn't WKB.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spatialite_blob(body: &[u8], srid: i32, extent: &Extent) -> Option<Vec<u8>> {
    let little = match body.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let class = read_u32(body.get(1..5)?, little)? & 0x0FFF_FFFF;
    if !(1..=7).contains(&(class % 1000)) || class / 1000 > 3 {
        return None;
    }
    let mut out = Vec::with_capacity(SPATIALITE_HEADER + 4 + body.len());
    out.push(0);
    out.push(1);
    out.extend_from_slice(&srid.to_le_bytes());
    for v in [extent.min_x, extent.max_x, extent.min_y, extent.max_y] {
        out.extend_from_slice(&(if extent.is_empty() { 0.0 } else { v }).to_le_bytes());
    }
    out.push(0x7C);
    out.extend_from_slice(&class.to_le_bytes());
    // The class carries the type, so the coordinates follow it without the WKB's own type.
    out.extend_from_slice(body.get(5..)?);
    Some(out)
}

/// A GeoPackage blob around a 2D little-endian WKB body: the `GP` header with an XY envelope and
/// the SRS.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn gpkg_blob(body: &[u8], srid: i32, extent: &Extent) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + body.len());
    out.extend_from_slice(b"GP");
    out.push(0);
    if extent.is_empty() {
        out.push(0b1_0001);
        out.extend_from_slice(&srid.to_le_bytes());
    } else {
        out.push(0b0000_0011);
        out.extend_from_slice(&srid.to_le_bytes());
        for v in [extent.min_x, extent.max_x, extent.min_y, extent.max_y] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(body);
    out
}

/// A PostGIS extended WKB body: the SRID flag in the type and the SRID after it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn ewkb(body: &[u8], srid: i32) -> Option<Vec<u8>> {
    let little = match body.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let ty = read_u32(body.get(1..5)?, little)?;
    if ty & 0x2000_0000 != 0 {
        return Some(body.to_vec());
    }
    let ty = ty | 0x2000_0000;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.push(u8::from(little));
    out.extend_from_slice(&if little { ty.to_le_bytes() } else { ty.to_be_bytes() });
    out.extend_from_slice(&if little { srid.to_le_bytes() } else { srid.to_be_bytes() });
    out.extend_from_slice(body.get(5..)?);
    Some(out)
}

/// A geometry blob split into its format, the envelope it declares (native coordinates) and the
/// WKB to decode. `None` for an empty or malformed GeoPackage blob (SpatiaLite and WKB bodies are
/// checked by decoding them).
pub(crate) fn split_blob(b: &[u8]) -> Option<(GeometryFormat, Option<Extent>, Cow<'_, [u8]>)> {
    if b.starts_with(b"GP") {
        if is_empty_blob(b) {
            return None;
        }
        let (envelope, body) = gpkg_split(b)?;
        return Some((GeometryFormat::Gpkg, envelope, Cow::Borrowed(body)));
    }
    if let Some(class) = spatialite_class(b) {
        // It is a SpatiaLite blob: one RustGIS can't read (empty, compressed) is refused rather
        // than decoded as WKB.
        let (envelope, body) = spatialite(b, class)?;
        return Some((GeometryFormat::SpatiaLite, envelope, body));
    }
    Some((GeometryFormat::Wkb, None, Cow::Borrowed(b)))
}

/// Whether a blob is an empty geometry (a GeoPackage one flagged so, or a SpatiaLite class 0).
pub(crate) fn is_empty(b: &[u8]) -> bool {
    is_empty_blob(b) || spatialite_class(b) == Some(0)
}

/// The SRID a blob declares: a GeoPackage header's, SpatiaLite's, or an EWKB geometry's.
pub(crate) fn srid(b: &[u8]) -> Option<i32> {
    if b.starts_with(b"GP") {
        let little = b.get(3)? & 1 != 0;
        return read_i32(b.get(4..8)?, little);
    }
    if let Some((s, _)) = spatialite_parts(b) {
        return Some(s);
    }
    let little = match b.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let ty = read_u32(b.get(1..5)?, little)?;
    // EWKB's SRID flag, with the code after the type.
    if ty & 0x2000_0000 == 0 {
        return None;
    }
    read_i32(b.get(5..9)?, little)
}

/// A SpatiaLite blob: the byte order, SRID, MBR and geometry class before the WKB body.
fn spatialite(b: &[u8], class: u32) -> Option<(Option<Extent>, Cow<'_, [u8]>)> {
    let (_srid, little) = spatialite_parts(b)?;
    // 1 point, 2 linestring, 3 polygon, 4 multipoint, 5 multilinestring, 6 multipolygon,
    // 7 geometry collection; the thousands carry Z and M, as ISO WKB does. Anything else (0, the
    // compressed classes) isn't read.
    let base = class % 1000;
    if !(1..=7).contains(&base) || class / 1000 > 3 {
        return None;
    }
    let f = |i: usize| -> Option<f64> {
        let a: [u8; 8] = b.get(6 + i * 8..14 + i * 8)?.try_into().ok()?;
        Some(if little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) })
    };
    // MBR order: min x, max x, min y, max y.
    let envelope = Extent::new(f(0)?, f(2)?, f(1)?, f(3)?);
    // The WKB is the byte order, the class bytes and the body after them.
    let mut body = Vec::with_capacity(b.len().saturating_sub(SPATIALITE_HEADER));
    body.push(u8::from(little));
    body.extend_from_slice(b.get(SPATIALITE_HEADER..)?);
    Some(((!envelope.is_empty()).then_some(envelope), Cow::Owned(body)))
}

/// The SRID and byte order of a SpatiaLite blob's header.
fn spatialite_parts(b: &[u8]) -> Option<(i32, bool)> {
    spatialite_class(b)?;
    let little = match b.get(1)? {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some((read_i32(b.get(2..6)?, little)?, little))
}

/// The geometry class of a SpatiaLite blob's header (`None` when the header isn't one).
fn spatialite_class(b: &[u8]) -> Option<u32> {
    if b.len() < SPATIALITE_HEADER + 4 || b.first() != Some(&0) || b.get(38) != Some(&0x7C) {
        return None;
    }
    let little = match b.get(1)? {
        0 => false,
        1 => true,
        _ => return None,
    };
    read_u32(b.get(SPATIALITE_HEADER..SPATIALITE_HEADER + 4)?, little)
}

fn read_u32(b: &[u8], little: bool) -> Option<u32> {
    let a: [u8; 4] = b.try_into().ok()?;
    Some(if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
}

fn read_i32(b: &[u8], little: bool) -> Option<i32> {
    let a: [u8; 4] = b.try_into().ok()?;
    Some(if little { i32::from_le_bytes(a) } else { i32::from_be_bytes(a) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spatialite(class: u32, srid: i32) -> Vec<u8> {
        let mut b = vec![0, 1];
        b.extend_from_slice(&srid.to_le_bytes());
        for v in [-10.0f64, 10.0, -5.0, 5.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0x7C);
        b.extend_from_slice(&class.to_le_bytes());
        // A POINT's body: x, y.
        b.extend_from_slice(&1.0f64.to_le_bytes());
        b.extend_from_slice(&2.0f64.to_le_bytes());
        b
    }

    #[test]
    fn spatialite_point_is_read() {
        let b = spatialite(1, 4326);
        let (format, envelope, body) = split_blob(&b).expect("a SpatiaLite point");
        assert_eq!(format, GeometryFormat::SpatiaLite);
        assert_eq!(envelope, Some(Extent::new(-10.0, -5.0, 10.0, 5.0)));
        let g = crate::wkb::decode(body.as_ref()).expect("a point");
        assert_eq!(g.kind(), crate::GeometryType::Point);
        assert_eq!(srid(&b), Some(4326));
    }

    #[test]
    fn spatialite_with_z_is_read() {
        let mut b = spatialite(1001, 4326);
        // POINT Z: x, y, z.
        b.extend_from_slice(&3.0f64.to_le_bytes());
        let (format, _, body) = split_blob(&b).expect("a SpatiaLite point Z");
        assert_eq!(format, GeometryFormat::SpatiaLite);
        assert!(crate::wkb::decode(body.as_ref()).is_some());
    }

    #[test]
    fn compressed_spatialite_is_refused() {
        let b = spatialite(100_0002, 4326);
        assert_eq!(split_blob(&b).map(|(f, _, _)| f), None);
    }

    #[test]
    fn malformed_spatialite_is_not_mistaken_for_wkb() {
        for class in [0, 8, u32::MAX] {
            let b = spatialite(class, 4326);
            assert_eq!(split_blob(&b).map(|(f, _, _)| f), None);
        }
        // A body that isn't WKB is refused rather than decoded into nonsense.
        let mut b = spatialite(1, 4326);
        b.truncate(SPATIALITE_HEADER);
        assert!(split_blob(&b).is_some());
        assert!(crate::wkb::decode(split_blob(&b).expect("split").2.as_ref()).is_none());
    }

    #[test]
    fn plain_wkb_is_read() {
        let g = crate::Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0));
        let b = crate::wkb::encode(&g);
        let (format, envelope, body) = split_blob(&b).expect("plain WKB");
        assert_eq!(format, GeometryFormat::Wkb);
        assert_eq!(envelope, None);
        assert_eq!(crate::wkb::decode(body.as_ref()), Some(g));
    }

    #[test]
    fn empty_spatialite_is_empty() {
        let b = spatialite(0, 4326);
        assert!(is_empty(&b));
        assert!(split_blob(&b).is_none());
    }

    #[test]
    fn shape_of_each_geometry_kind() {
        use crate::Geometry;
        let cases = [
            (Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0)), Shape::Point),
            (Geometry::MultiPoint(vec![rustgis_geom::Coord::new(1.0, 2.0)]), Shape::MultiPoint),
            (Geometry::Line(vec![vec![rustgis_geom::Coord::new(1.0, 2.0), rustgis_geom::Coord::new(3.0, 4.0)]]), Shape::MultiLine),
            (
                Geometry::Polygon(vec![vec![vec![
                    rustgis_geom::Coord::new(1.0, 2.0),
                    rustgis_geom::Coord::new(3.0, 4.0),
                    rustgis_geom::Coord::new(5.0, 6.0),
                ]]]),
                Shape::MultiPolygon,
            ),
        ];
        for (g, shape) in cases {
            let body = crate::wkb::encode(&g);
            assert_eq!(Shape::of(&body), Some(shape), "{shape:?}");
            assert!(Shape::of(&[0xFF; 4]).is_none());
        }
        let single =
            crate::wkb::encode_single(&Geometry::Line(vec![vec![rustgis_geom::Coord::new(1.0, 2.0), rustgis_geom::Coord::new(3.0, 4.0)]]));
        assert_eq!(single.as_deref().and_then(Shape::of), Some(Shape::Line));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn written_blobs_read_back() {
        use crate::Geometry;
        let g = Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0));
        let body = crate::wkb::encode(&g);
        let extent = g.extent();

        let b = spatialite_blob(&body, 4326, &extent).expect("a SpatiaLite blob");
        let (format, envelope, read) = split_blob(&b).expect("a SpatiaLite blob reads");
        assert_eq!(format, GeometryFormat::SpatiaLite);
        assert_eq!(envelope, Some(extent));
        assert_eq!(srid(&b), Some(4326));
        assert_eq!(crate::wkb::decode(read.as_ref()), Some(g.clone()));

        let b = gpkg_blob(&body, 4326, &extent);
        let (format, envelope, read) = split_blob(&b).expect("a GeoPackage blob reads");
        assert_eq!(format, GeometryFormat::Gpkg);
        assert_eq!(envelope, Some(extent));
        assert_eq!(srid(&b), Some(4326));
        assert_eq!(crate::wkb::decode(read.as_ref()), Some(g));

        let b = ewkb(&body, 4326).expect("an EWKB body");
        assert_eq!(srid(&b), Some(4326));
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let samples: Vec<Vec<u8>> = vec![
            vec![0; 43],
            vec![1; 43],
            vec![
                0x00, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0x7C, 1, 0, 0, 0,
            ],
            vec![
                0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x7C, 1,
                0, 0, 0,
            ],
        ];
        for b in samples {
            let _ = split_blob(&b);
            let _ = is_empty(&b);
            let _ = srid(&b);
        }
    }
}
