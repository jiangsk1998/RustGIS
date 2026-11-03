//! A table of an ordinary SQLite database held in memory, read with the pure-Rust SQLite reader
//! (`crate::sqlite`): how the browser opens SQLite files, where SQLite (C) isn't available.
//!
//! Geometry is detected as on the desktop (see [`super::sqlite_detect`]); reads scan the table,
//! testing each blob's envelope before decoding it, and an attributes table is read as it is.

use std::ops::ControlFlow;

use rustgis_geom::Extent;

use super::sqlite_blob;
use super::sqlite_detect::{self, ColumnDecl, Named};
use super::{Batch, FeatureSource, Reproject, Request, Sampler, Shared, SourceInfo, clamp_limit, in_request};
use crate::sqlite::{Db, Table, Val};
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, pictures, wkb};

/// Rows scanned for a table's extent (and noted when the table has more).
const SAMPLE: usize = 4096;

/// A feature table of an SQLite database held in memory.
pub struct SqliteMemSource {
    info: SourceInfo,
    db: Db<Shared>,
    table: Table,
    /// The geometry column, when the table has one.
    geom_col: Option<usize>,
    /// Column index of each field, in `info.fields` order.
    field_cols: Vec<usize>,
    reproject: Reproject,
}

fn err(e: String) -> DataError {
    DataError::Format(format!("SQLite: {e}"))
}

/// Whether a table is SQLite's own or one of a GeoPackage's or SpatiaLite's.
fn internal(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["sqlite_", "rtree_", "gpkg_", "spatialite_", "views_"].iter().any(|p| n.starts_with(p))
}

/// The tables of an SQLite database a person reads.
fn user_tables(tables: &[Table]) -> Vec<String> {
    let mut names: Vec<String> = tables.iter().filter(|t| !internal(&t.name)).map(|t| t.name.clone()).collect();
    names.sort();
    names
}

/// The rows of a metadata table, as (column name → value) lookups; `None` when it isn't there (or
/// can't be read).
fn meta_rows(db: &Db<Shared>, tables: &[Table], name: &str) -> Option<Vec<Vec<(String, Val)>>> {
    let t = tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
    let mut out = Vec::new();
    let mut too_many = false;
    db.rows(t, |_, r| {
        out.push(t.columns.iter().map(|c| c.name.to_ascii_lowercase()).zip(r.iter().cloned()).collect());
        // Metadata tables are small; a huge one is not a GeoPackage's or SpatiaLite's.
        too_many = out.len() > 100_000;
        if too_many { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .ok()?;
    if too_many {
        return None;
    }
    Some(out)
}

fn get<'a>(row: &'a [(String, Val)], col: &str) -> Option<&'a Val> {
    row.iter().find(|(c, _)| c == col).map(|(_, v)| v)
}

fn text(v: Option<&Val>) -> Option<String> {
    match v? {
        Val::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn int(v: Option<&Val>) -> Option<i64> {
    match v? {
        Val::Int(i) => Some(*i),
        Val::Real(x) if x.fract() == 0.0 && x.abs() < 9e15 => Some(*x as i64),
        _ => None,
    }
}

/// Geometry a GeoPackage's metadata declares for the table (a GeoPackage named `.sqlite`).
fn gpkg_named(db: &Db<Shared>, schema: &[Table], table: &str) -> Option<Named> {
    let rows = meta_rows(db, schema, "gpkg_geometry_columns")?;
    let r = rows.iter().find(|r| text(get(r, "table_name")).is_some_and(|t| t.eq_ignore_ascii_case(table)))?;
    Some(Named {
        column: text(get(r, "column_name"))?,
        srid: int(get(r, "srs_id")).and_then(|s| i32::try_from(s).ok()),
        geometry_type: text(get(r, "geometry_type_name")),
    })
}

/// Geometry a SpatiaLite's `geometry_columns` declares for the table.
fn spatialite_named(db: &Db<Shared>, schema: &[Table], table: &str) -> Option<Named> {
    let rows = meta_rows(db, schema, "geometry_columns")?;
    let r = rows.iter().find(|r| text(get(r, "f_table_name")).is_some_and(|t| t.eq_ignore_ascii_case(table)))?;
    Some(Named {
        column: text(get(r, "f_geometry_column"))?,
        srid: int(get(r, "srid")).and_then(|s| i32::try_from(s).ok()),
        geometry_type: int(get(r, "geometry_type")).map(|t| t.to_string()),
    })
}

/// The EPSG code an SRID means, from a SpatiaLite `spatial_ref_sys` when it has one; a positive
/// SRID with no such row is taken as an EPSG code, as writers usually mean by it.
fn epsg_of(db: &Db<Shared>, schema: &[Table], srid: i32, notes: &mut Vec<String>) -> Option<u32> {
    let Some(t) = schema.iter().find(|t| t.name.eq_ignore_ascii_case("spatial_ref_sys")) else {
        return u32::try_from(srid).ok();
    };
    let mut found = None;
    let _ = db.rows(t, |_, r| {
        let row: Vec<(String, Val)> = t.columns.iter().map(|c| c.name.to_ascii_lowercase()).zip(r.iter().cloned()).collect();
        if int(get(&row, "srid")) == Some(i64::from(srid)) {
            found = Some((text(get(&row, "auth_name")), int(get(&row, "auth_srid"))));
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    match found {
        Some((Some(org), Some(id))) if org.eq_ignore_ascii_case("epsg") => u32::try_from(id).ok(),
        Some(_) => {
            notes.push(format!("its coordinate system (SRID {srid}) has no EPSG code; read as longitude/latitude"));
            None
        }
        None => u32::try_from(srid).ok(),
    }
}

/// The tables of an SQLite database held in memory, in the order the desktop lists them.
pub fn layers(bytes: Shared) -> Result<Vec<String>> {
    let db = Db::open(bytes).map_err(err)?;
    Ok(user_tables(&db.tables().map_err(err)?))
}

/// A table's first few values of each column, for geometry detection (one bounded scan).
fn samples(db: &Db<Shared>, table: &Table) -> Vec<(String, Vec<Vec<u8>>)> {
    let mut out: Vec<(String, Vec<Vec<u8>>)> = table.columns.iter().map(|c| (c.name.clone(), Vec::new())).collect();
    let mut looked = 0;
    let _ = db.rows(table, |_, r| {
        looked += 1;
        for (i, (_, values)) in out.iter_mut().enumerate() {
            if values.len() >= 16 {
                continue;
            }
            if let Some(Val::Blob(b)) = r.get(i)
                && !b.is_empty()
            {
                values.push(b.clone());
            }
        }
        if looked >= 256 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    });
    out
}

impl SqliteMemSource {
    /// Opens a table (`layer`, else the first) of the database in `bytes`.
    pub fn open_shared(bytes: Shared, layer: Option<&str>) -> Result<Self> {
        let db = Db::open(bytes).map_err(err)?;
        let schema = db.tables().map_err(err)?;
        let tables = user_tables(&schema);
        let table_name = match layer {
            Some(l) => tables
                .iter()
                .find(|t| t.eq_ignore_ascii_case(l))
                .cloned()
                .ok_or_else(|| DataError::Format(format!("no table '{l}' (tables: {})", tables.join(", "))))?,
            None => tables.first().cloned().ok_or_else(|| DataError::Format("the database has no tables".into()))?,
        };
        let table = schema
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&table_name))
            .cloned()
            .ok_or_else(|| DataError::Format(format!("SQLite: no table '{table_name}'")))?;
        let mut notes = Vec::new();
        if layer.is_none() && tables.len() > 1 {
            notes.push(format!("the first of {} tables ({})", tables.len(), tables.join(", ")));
        }
        let cols: Vec<ColumnDecl> = table.columns.iter().map(|c| ColumnDecl { name: c.name.clone(), decl: c.decl.clone() }).collect();
        let named = gpkg_named(&db, &schema, &table_name).or_else(|| spatialite_named(&db, &schema, &table_name));
        let sample = samples(&db, &table);
        let detection = sqlite_detect::detect(&cols, named, |column| {
            sample.iter().find(|(name, _)| name.eq_ignore_ascii_case(column)).map(|(_, v)| v.clone()).unwrap_or_default()
        });
        notes.extend(detection.notes);
        let geom_col = detection.geometry.as_ref().and_then(|g| table.columns.iter().position(|c| c.name.eq_ignore_ascii_case(&g.column)));
        let (epsg, srid) = match detection.geometry.as_ref().and_then(|g| g.srid) {
            Some(s) => (epsg_of(&db, &schema, s, &mut notes), Some(s)),
            None => (None, None),
        };
        if detection.geometry.is_some() && srid.is_none() {
            notes.push("its geometry declares no coordinate system; read as longitude/latitude".into());
        }
        let reproject = Reproject::from_epsg(epsg, &mut notes);

        let mut fields = Vec::new();
        let mut field_cols = Vec::new();
        let mut dropped = Vec::new();
        for (i, c) in table.columns.iter().enumerate() {
            if Some(i) == geom_col || Some(i) == table.rowid_column {
                continue;
            }
            let Some(ty) = super::gpkg_blob::field_type(&c.decl) else {
                continue;
            };
            if ty == FieldType::Image {
                let (pictured, others) = probe_pictures(&db, &table, i);
                if pictured == 0 && others > 0 {
                    dropped.push(c.name.clone());
                    continue;
                }
                if others > 0 {
                    notes.push(super::mixed_pictures_note(others, pictured + others, &c.name));
                }
            }
            fields.push(Field::new(c.name.clone(), ty));
            field_cols.push(i);
        }
        if !dropped.is_empty() {
            notes.push(super::not_pictures_note(&dropped));
        }

        let count = db.count(table.root).map_err(err)?;
        let geometry_type = detection.geometry.as_ref().map(|g| g.geometry_type).unwrap_or(GeometryType::None);
        let zm = detection.geometry.as_ref().is_some_and(|g| g.zm);
        let mut info = SourceInfo {
            name: table_name,
            format: "SQLite",
            geometry_type,
            fields,
            extent: Extent::EMPTY,
            count: Some(count),
            crs: reproject.label(),
            indexed: false,
            notes,
            zm_dropped: false,
        };
        if zm {
            info.drop_zm(None);
        }
        let mut src = SqliteMemSource { info, db, table, geom_col, field_cols, reproject };
        if let Some(g) = geom_col {
            if count > SAMPLE as u64 {
                let n = count;
                src.info.notes.push(format!("its extent comes from the first {SAMPLE} features of {n}"));
            }
            let native = src.envelopes(g);
            src.info.extent = src.reproject.extent_to_lonlat(&native);
        }
        Ok(src)
    }

    /// The union of the envelopes in the first [`SAMPLE`] geometries (native coordinates).
    fn envelopes(&self, col: usize) -> Extent {
        let mut e = Extent::EMPTY;
        let mut looked = 0;
        let _ = self.db.rows(&self.table, |_, r| {
            looked += 1;
            if let Some(Val::Blob(b)) = r.get(col)
                && let Some((_, envelope, body)) = sqlite_blob::split_blob(b)
            {
                match envelope {
                    Some(x) => e = e.union(&x),
                    None => {
                        if let Some(g) = wkb::decode(body.as_ref()) {
                            e = e.union(&g.extent());
                        }
                    }
                }
            }
            if looked >= SAMPLE { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        e
    }

    /// A row's field values, in `info.fields` order.
    fn attrs(&self, r: &[Val]) -> Vec<Value> {
        self.info.fields.iter().zip(&self.field_cols).map(|(f, &c)| r.get(c).map(|v| value(v, f.ty)).unwrap_or(Value::Null)).collect()
    }

    /// The original bytes of one row's image column.
    fn image_at(&self, fid: u64, field: &str) -> Result<Vec<u8>> {
        let at = self
            .info
            .fields
            .iter()
            .position(|f| f.name == field && f.ty == FieldType::Image)
            .ok_or_else(|| DataError::Unsupported(format!("{field} is not an image field of {}", self.info.name)))?;
        let col = *self.field_cols.get(at).ok_or_else(|| DataError::Format(format!("{field} has no column in {}", self.info.name)))?;
        let mut found = None;
        self.db
            .rows(&self.table, |rowid, r| {
                if rowid.cast_unsigned() != fid {
                    return ControlFlow::Continue(());
                }
                found = match r.get(col) {
                    Some(Val::Blob(b)) if !b.is_empty() => Some(b.to_vec()),
                    _ => None,
                };
                ControlFlow::Break(())
            })
            .map_err(err)?;
        found.ok_or_else(|| DataError::Format(format!("the image field {field} of row {fid} is empty or the row is gone")))
    }
}

/// Pictures and other values among a column's first few values (at most 64, looking at no more
/// than 256 rows).
fn probe_pictures(db: &Db<Shared>, table: &Table, col: usize) -> (usize, usize) {
    let (mut pictured, mut others, mut looked) = (0, 0, 0);
    let _ = db.rows(table, |_, r| {
        looked += 1;
        match r.get(col) {
            Some(Val::Blob(b)) if pictures::meta(b).is_some() => pictured += 1,
            Some(Val::Null) | None => {}
            Some(_) => others += 1,
        }
        if pictured + others >= 64 || looked >= 256 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    });
    (pictured, others)
}

/// An attribute value of a field's type.
fn value(v: &Val, ty: FieldType) -> Value {
    match (v, ty) {
        (Val::Null, _) => Value::Null,
        (Val::Blob(b), _) => {
            pictures::meta(b).map(|(f, w, h)| Value::Image(crate::ImageValue::new(f.mime(), w, h, b.len() as u64))).unwrap_or(Value::Null)
        }
        (Val::Int(i), FieldType::Boolean) => Value::Bool(*i != 0),
        (Val::Int(i), FieldType::Double) => Value::Float(*i as f64),
        (Val::Int(i), _) => Value::Int(*i),
        (Val::Real(f), FieldType::Integer) if f.fract() == 0.0 && f.abs() < 9e15 => Value::Int(*f as i64),
        (Val::Real(f), _) => Value::Float(*f),
        (Val::Text(t), FieldType::Text) => Value::Text(t.clone()),
        (Val::Text(t), ty) => Value::parse(t, ty),
    }
}

impl FeatureSource for SqliteMemSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn image(&self, fid: u64, field: &str) -> Result<Vec<u8>> {
        self.image_at(fid, field)
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let Some(geom_col) = self.geom_col else {
            // An attributes table: every row is a feature, whatever the extent.
            let mut sample = Sampler::with_total(limit, self.info.count.unwrap_or(0));
            self.db
                .rows(&self.table, |rowid, r| {
                    if !sample.wants() {
                        return ControlFlow::Continue(());
                    }
                    sample.keep(Feature { id: rowid as u64, geometry: None, attrs: self.attrs(r) });
                    ControlFlow::Continue(())
                })
                .map_err(err)?;
            return Ok(Batch { matched: sample.seen(), complete: sample.complete(), features: sample.items, skipped: 0 });
        };
        let native = req.extent.map(|e| self.reproject.extent_from_lonlat(&e));
        let mut sample = if native.is_none() { Sampler::with_total(limit, self.info.count.unwrap_or(0)) } else { Sampler::new(limit) };
        let mut skipped = 0;
        self.db
            .rows(&self.table, |rowid, r| {
                let blob = match r.get(geom_col) {
                    Some(Val::Blob(b)) => b,
                    Some(Val::Null) | None => return ControlFlow::Continue(()),
                    Some(_) => {
                        skipped += 1;
                        return ControlFlow::Continue(());
                    }
                };
                if sqlite_blob::is_empty(blob) {
                    return ControlFlow::Continue(());
                }
                let Some((_, envelope, body)) = sqlite_blob::split_blob(blob) else {
                    skipped += 1;
                    return ControlFlow::Continue(());
                };
                if let (Some(q), Some(e)) = (&native, envelope)
                    && !e.intersects(q)
                {
                    return ControlFlow::Continue(());
                }
                let known = native.is_none() || envelope.is_some();
                if known && !sample.wants() {
                    return ControlFlow::Continue(());
                }
                let Some(geometry) = wkb::decode(body.as_ref()).and_then(|g| self.reproject.geometry(g)) else {
                    skipped += 1;
                    return ControlFlow::Continue(());
                };
                if !in_request(&geometry, req) || (!known && !sample.wants()) {
                    return ControlFlow::Continue(());
                }
                // Integer keys, negative ones too, map to distinct ids.
                sample.keep(Feature { id: rowid as u64, geometry: Some(geometry), attrs: self.attrs(r) });
                ControlFlow::Continue(())
            })
            .map_err(err)?;
        Ok(Batch { matched: sample.seen(), complete: sample.complete(), features: sample.items, skipped })
    }

    /// The upload is held in memory already, so every table can be visited.
    fn visits_chunks(&self) -> bool {
        true
    }

    /// A scan in table order, keeping and skipping the rows a whole read does.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let req = Request { extent, limit: usize::MAX };
        let native = match (self.geom_col, extent) {
            (Some(_), Some(e)) => Some(self.reproject.extent_from_lonlat(&e)),
            _ => None,
        };
        let mut out = Batch { complete: true, ..Batch::default() };
        // The visitor's error, which stops the scan.
        let mut failed = None;
        self.db
            .rows(&self.table, |rowid, r| {
                let geometry = match self.geom_col {
                    Some(col) => {
                        let blob = match r.get(col) {
                            Some(Val::Blob(b)) => b,
                            Some(Val::Null) | None => return ControlFlow::Continue(()),
                            Some(_) => {
                                out.skipped += 1;
                                return ControlFlow::Continue(());
                            }
                        };
                        if sqlite_blob::is_empty(blob) {
                            return ControlFlow::Continue(());
                        }
                        let Some((_, envelope, body)) = sqlite_blob::split_blob(blob) else {
                            out.skipped += 1;
                            return ControlFlow::Continue(());
                        };
                        if let (Some(q), Some(e)) = (&native, envelope)
                            && !e.intersects(q)
                        {
                            return ControlFlow::Continue(());
                        }
                        let Some(g) = wkb::decode(body.as_ref()).and_then(|g| self.reproject.geometry(g)) else {
                            out.skipped += 1;
                            return ControlFlow::Continue(());
                        };
                        if !in_request(&g, &req) {
                            return ControlFlow::Continue(());
                        }
                        Some(g)
                    }
                    None => None,
                };
                out.matched += 1;
                match chunks.push(Feature { id: rowid as u64, geometry, attrs: self.attrs(r) }) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => {
                        failed = Some(e);
                        ControlFlow::Break(())
                    }
                }
            })
            .map_err(err)?;
        if let Some(e) = failed {
            return Err(e);
        }
        chunks.flush()?;
        Ok(out)
    }
}
