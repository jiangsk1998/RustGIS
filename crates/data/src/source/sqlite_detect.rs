//! Which column of a general SQLite table holds geometry, and how: from GeoPackage or SpatiaLite
//! metadata, else from the declared column types and a sample of their values. Shared by the
//! desktop reader and the in-memory one.

use super::sqlite_blob;
use crate::{GeometryType, wkb};

/// A column of the table, as SQLite declares it.
#[derive(Clone, Debug)]
pub(crate) struct ColumnDecl {
    pub name: String,
    pub decl: String,
}

/// Geometry a table's metadata declares.
#[derive(Clone, Debug)]
pub(crate) struct Named {
    pub column: String,
    pub srid: Option<i32>,
    /// The geometry type the metadata names (`POINT`, `1`…).
    pub geometry_type: Option<String>,
}

/// Geometry found in a table.
#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    pub column: String,
    /// A positive SRID, when the blob or metadata declares one.
    pub srid: Option<i32>,
    pub geometry_type: GeometryType,
    /// Whether the sampled geometries carry Z or M values, which reading drops.
    pub zm: bool,
}

/// What detection found, and what it left out.
#[derive(Clone, Debug)]
pub(crate) struct Detection {
    pub geometry: Option<Geometry>,
    pub notes: Vec<String>,
}

/// A sampled geometry column.
struct Classified {
    srid: Option<i32>,
    geometry_type: GeometryType,
    zm: bool,
}

impl Classified {
    fn into_geometry(self, column: &str) -> Geometry {
        Geometry { column: column.to_string(), srid: self.srid, geometry_type: self.geometry_type, zm: self.zm }
    }
}

/// Detects a table's geometry column from `named` metadata, else from the columns' declared types
/// and their `sample` values (up to 16 non-NULL ones, looked at in order).
pub(crate) fn detect(cols: &[ColumnDecl], named: Option<Named>, mut sample: impl FnMut(&str) -> Vec<Vec<u8>>) -> Detection {
    let mut notes = Vec::new();
    if let Some(named) = named {
        let Some(col) = cols.iter().find(|c| c.name.eq_ignore_ascii_case(&named.column)) else {
            notes.push(format!("its metadata names a geometry column ({}) the table doesn't have", named.column));
            return Detection { geometry: None, notes };
        };
        let found = classify(&sample(&col.name));
        let geometry = Geometry {
            column: col.name.clone(),
            srid: named.srid.or(found.as_ref().and_then(|f| f.srid)),
            geometry_type: named
                .geometry_type
                .as_deref()
                .map(geometry_type_of)
                .filter(|t| *t != GeometryType::None)
                .or_else(|| found.as_ref().map(|f| f.geometry_type))
                .unwrap_or(GeometryType::None),
            zm: found.is_some_and(|f| f.zm),
        };
        return Detection { geometry: Some(geometry), notes };
    }

    // Columns declared as geometry, then binary ones that may hold it without saying.
    let declared: Vec<&ColumnDecl> = cols.iter().filter(|c| looks_like_geometry(&c.decl)).collect();
    let binary: Vec<&ColumnDecl> = cols
        .iter()
        .filter(|c| !looks_like_geometry(&c.decl) && (c.decl.is_empty() || c.decl.to_ascii_uppercase().contains("BLOB")))
        .collect();
    let mut chosen: Option<Geometry> = None;
    let mut extras: Vec<String> = Vec::new();
    let mut first_declared: Option<&ColumnDecl> = None;
    for c in &declared {
        let samples = sample(&c.name);
        if let Some(found) = classify(&samples) {
            if chosen.is_some() {
                extras.push(c.name.clone());
                continue;
            }
            chosen = Some(found.into_geometry(&c.name));
        } else if samples.is_empty() && first_declared.is_none() {
            first_declared = Some(c);
        }
    }
    // A column declared as geometry, in an empty table (or one whose values aren't read): it is
    // one, in plain WKB, with the type its declaration names.
    if chosen.is_none()
        && let Some(c) = first_declared
    {
        chosen = Some(Geometry { column: c.name.clone(), srid: None, geometry_type: geometry_type_of(&c.decl), zm: false });
    }
    for c in &binary {
        let samples = sample(&c.name);
        if let Some(found) = classify(&samples) {
            if chosen.is_some() {
                extras.push(c.name.clone());
            } else {
                chosen = Some(found.into_geometry(&c.name));
            }
        }
    }
    if !extras.is_empty() {
        let plural = if extras.len() == 1 { "" } else { "s" };
        let verb = if extras.len() == 1 { "is" } else { "are" };
        notes.push(format!("{} other geometry column{plural} {verb} not read: {}", extras.len(), extras.join(", ")));
    }
    if chosen.is_none() && !declared.is_empty() {
        let names: Vec<&str> = declared.iter().map(|c| c.name.as_str()).collect();
        let plural = if names.len() == 1 { "" } else { "s" };
        notes.push(format!("the column{plural} declared as geometry hold{plural} no geometry RustGIS can read: {}", names.join(", ")));
    }
    Detection { geometry: chosen, notes }
}

/// The first of the sampled blobs that is a geometry, and how it is stored.
fn classify(samples: &[Vec<u8>]) -> Option<Classified> {
    for b in samples {
        if sqlite_blob::is_empty(b) {
            continue;
        }
        let Some((_format, _envelope, body)) = sqlite_blob::split_blob(b) else { continue };
        let Some(g) = wkb::decode(body.as_ref()) else { continue };
        let zm = wkb::has_zm(body.as_ref());
        return Some(Classified { srid: sqlite_blob::srid(b).filter(|s| *s > 0), geometry_type: g.kind(), zm });
    }
    None
}

/// Whether a declared type names geometry.
fn looks_like_geometry(decl: &str) -> bool {
    let d = decl.to_ascii_uppercase();
    ["GEOM", "POINT", "LINE", "POLY", "MULTI", "CURVE", "SURFACE"].iter().any(|k| d.contains(k))
}

/// The geometry type a declared type or metadata names: a name (`POINT`, `MULTIPOLYGON`…), a
/// SpatiaLite code (`1`, `2`…) or `GEOMETRY` (mixed).
pub(crate) fn geometry_type_of(name: &str) -> GeometryType {
    let n = name.trim().to_ascii_uppercase();
    match n.as_str() {
        "1" | "4" => return GeometryType::Point,
        "2" | "5" => return GeometryType::Polyline,
        "3" | "6" => return GeometryType::Polygon,
        _ => {}
    }
    if n.contains("POINT") {
        GeometryType::Point
    } else if n.contains("LINE") || n.contains("CURVE") {
        GeometryType::Polyline
    } else if n.contains("POLYGON") || n.contains("SURFACE") {
        GeometryType::Polygon
    } else {
        GeometryType::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_geom::Coord;

    fn col(name: &str, decl: &str) -> ColumnDecl {
        ColumnDecl { name: name.into(), decl: decl.into() }
    }

    fn point() -> Vec<u8> {
        wkb::encode(&crate::Geometry::Point(Coord::new(1.0, 2.0)))
    }

    fn spatialite_point() -> Vec<u8> {
        let mut b = vec![0u8, 1];
        b.extend_from_slice(&4326i32.to_le_bytes());
        for v in [1.0f64, 1.0, 2.0, 2.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0x7C);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1.0f64.to_le_bytes());
        b.extend_from_slice(&2.0f64.to_le_bytes());
        b
    }

    #[test]
    fn plain_wkb_in_a_blob_column_is_geometry() {
        let cols = [col("id", "INTEGER"), col("geom", "BLOB"), col("photo", "BLOB")];
        let mut sample = |name: &str| match name {
            "geom" => vec![point()],
            "photo" => vec![vec![0x89, b'P', b'N', b'G']],
            _ => Vec::new(),
        };
        let d = detect(&cols, None, &mut sample);
        let g = d.geometry.expect("a geometry column");
        assert_eq!(g.column, "geom");
        assert_eq!(g.geometry_type, GeometryType::Point);
        assert!(!g.zm);
        assert!(d.notes.is_empty());
    }

    #[test]
    fn a_table_without_geometry_has_none() {
        let cols = [col("id", "INTEGER"), col("photo", "BLOB")];
        let mut sample = |name: &str| match name {
            "photo" => vec![vec![0x89, b'P', b'N', b'G']],
            _ => Vec::new(),
        };
        let d = detect(&cols, None, &mut sample);
        assert!(d.geometry.is_none());
    }

    #[test]
    fn spatialite_metadata_wins() {
        let cols = [col("id", "INTEGER"), col("geom", "BLOB")];
        let named = Named { column: "geom".into(), srid: Some(4326), geometry_type: Some("1".into()) };
        let mut sample = |name: &str| if name == "geom" { vec![spatialite_point()] } else { Vec::new() };
        let d = detect(&cols, Some(named), &mut sample);
        let g = d.geometry.expect("a geometry column");
        assert_eq!(g.srid, Some(4326));
        assert_eq!(g.geometry_type, GeometryType::Point);
    }

    #[test]
    fn extra_geometry_columns_are_named() {
        let cols = [col("one", "POINT"), col("two", "POINT")];
        let mut sample = |_name: &str| vec![point()];
        let d = detect(&cols, None, &mut sample);
        assert_eq!(d.geometry.expect("a column").column, "one");
        assert_eq!(d.notes.len(), 1);
        assert!(d.notes[0].contains("two"));
    }

    #[test]
    fn hostile_blobs_are_not_geometry() {
        let cols = [col("geom", "BLOB")];
        let mut sample = |_name: &str| vec![vec![0xFF; 43], vec![0; 12]];
        let d = detect(&cols, None, &mut sample);
        assert!(d.geometry.is_none());
    }
}
