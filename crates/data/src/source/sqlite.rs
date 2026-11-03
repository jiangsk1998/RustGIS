//! A table of an ordinary SQLite database, read with SQLite (desktop only).
//!
//! Unlike a GeoPackage, such a table declares nothing about its geometry: the reader detects it
//! from SpatiaLite or GeoPackage metadata, the declared column types and a sample of the values
//! (see [`super::sqlite_detect`]). A table with no geometry is read as attributes (pictures kept
//! as image fields either way). Extent queries scan the table, testing each blob's envelope before
//! decoding it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use rustgis_geom::Extent;

pub mod edit;

use super::sqlite_blob;
use super::sqlite_detect::{self, ColumnDecl, Named};
use super::{Batch, FeatureSource, Reproject, Request, Sampler, SourceInfo, clamp_limit, in_request};
use crate::source::gpkg::{quote as gpkg_quote, value as gpkg_value};
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};

/// Rows sampled for a table's extent and geometry column.
const SAMPLE: usize = 4096;

/// A feature table of an SQLite database.
pub struct SqliteSource {
    info: SourceInfo,
    /// `None` only while the source is dropped.
    conn: Mutex<Option<Connection>>,
    table: String,
    /// The `INTEGER PRIMARY KEY` column, else `None` for the rowid.
    key: Option<String>,
    /// The geometry column, when the table has one.
    geometry: Option<String>,
    /// The SRID the geometry declares, if any; reading without one assumes longitude/latitude.
    srid: Option<i32>,
    /// Whether the sampled geometries carry Z or M values, which reading drops.
    zm: bool,
    reproject: Reproject,
    /// A copy written for a file opened from memory, removed when the source is dropped.
    temp: Option<PathBuf>,
    /// The database file and its identity when opened, for a traversal on a connection of its own.
    file: Option<(PathBuf, Option<(u64, u64)>)>,
}

impl Drop for SqliteSource {
    fn drop(&mut self) {
        // Closed first: Windows can't remove a file SQLite still holds open.
        if let Ok(c) = self.conn.get_mut() {
            drop(c.take());
        }
        if let Some(p) = &self.temp {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn err(e: rusqlite::Error) -> DataError {
    DataError::Format(format!("SQLite: {e}"))
}

fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(err)?;
    // A cap on SQLite's page cache, so a huge database can't take all the memory; and the file is
    // hostile: nothing it defines (views, triggers, generated columns) may call functions, and
    // nothing writes.
    let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false);
    let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
    let _ = conn.execute_batch("PRAGMA cache_size = -65536; PRAGMA query_only = ON;");
    Ok(conn)
}

/// The tables of an SQLite database a person reads, leaving out SQLite's own and the metadata,
/// index and helper tables of GeoPackage and SpatiaLite.
pub fn tables(path: &Path) -> Result<Vec<String>> {
    user_tables(&open_db(path)?)
}

fn user_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table'
             AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'rtree_%' AND name NOT LIKE 'gpkg_%'
             AND name NOT LIKE 'spatialite_%' AND name NOT LIKE 'views_%' ORDER BY name",
        )
        .map_err(err)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

impl SqliteSource {
    /// Opens a table (`layer`, else the first) of the database at `path`.
    pub fn open(path: &Path, layer: Option<&str>) -> Result<Self> {
        // The identity is taken before the database is opened: a file put in its place after this
        // leaves it describing the file opened, which a traversal then refuses to swap.
        let id = file_identity(path);
        Self::open_conn(open_db(path)?, layer, None, id)
    }

    /// Opens a database held in memory, through a temporary copy (SQLite reads files).
    pub fn open_bytes(bytes: &[u8], layer: Option<&str>) -> Result<Self> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let mut tries = 0;
        let (p, mut f) = loop {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let salt = std::collections::hash_map::RandomState::new();
            let tag = std::hash::BuildHasher::hash_one(&salt, (nanos, N.fetch_add(1, Ordering::Relaxed)));
            let p = std::env::temp_dir().join(format!("rustgis-{}-{tag:016x}.sqlite", std::process::id()));
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            // Readable by this user only: the copy holds their data, in a shared folder.
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            match o.open(&p) {
                Ok(f) => break (p, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
                Err(e) => return Err(e.into()),
            }
        };
        if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
            let _ = std::fs::remove_file(&p);
            return Err(e.into());
        }
        drop(f);
        let id = file_identity(&p);
        match open_db(&p).and_then(|c| Self::open_conn(c, layer, Some(p.clone()), id)) {
            Ok(s) => Ok(s),
            Err(e) => {
                let _ = std::fs::remove_file(&p);
                Err(e)
            }
        }
    }

    /// Opens a table (`layer`, else the first) on an open database, detecting its geometry and
    /// noting what reading changes: an assumed coordinate system, other tables, Z or M values
    /// dropped, binary columns whose values aren't pictures.
    fn open_conn(conn: Connection, layer: Option<&str>, temp: Option<PathBuf>, id: Option<(u64, u64)>) -> Result<Self> {
        let tables = user_tables(&conn)?;
        let table = match layer {
            Some(l) => tables
                .iter()
                .find(|t| t.eq_ignore_ascii_case(l))
                .cloned()
                .ok_or_else(|| DataError::Format(format!("no table '{l}' (tables: {})", tables.join(", "))))?,
            None => tables.first().cloned().ok_or_else(|| DataError::Format("the database has no tables".into()))?,
        };
        let mut notes = Vec::new();
        if layer.is_none() && tables.len() > 1 {
            notes.push(format!("the first of {} tables ({})", tables.len(), tables.join(", ")));
        }
        // The primary key, else the rowid.
        let (cols, key) = columns(&conn, &table)?;
        if key.is_none() && without_rowid(&conn, &table)? {
            return Err(DataError::Format(format!("{table} is a WITHOUT ROWID table, whose rows have no id RustGIS can use")));
        }
        let named = gpkg_named(&conn, &table).or_else(|| spatialite_named(&conn, &table));
        let detection = sqlite_detect::detect(&cols, named, |column| sample_blobs(&conn, &table, column));
        notes.extend(detection.notes);
        let geometry = detection.geometry.as_ref().map(|g| g.column.clone());
        let (epsg, srid) = match detection.geometry.as_ref().and_then(|g| g.srid) {
            Some(s) => (epsg_of(&conn, s, &mut notes), Some(s)),
            None => (None, None),
        };
        if detection.geometry.is_some() && srid.is_none() {
            notes.push("its geometry declares no coordinate system; read as longitude/latitude".into());
        }
        let reproject = Reproject::from_epsg(epsg, &mut notes);

        // The fields: every column but the key and geometry, binary ones only when they hold
        // pictures (or say so to their readers).
        let mut fields = Vec::new();
        let mut dropped = Vec::new();
        for c in &cols {
            if Some(&c.name) == geometry.as_ref() || Some(&c.name) == key.as_ref() {
                continue;
            }
            let Some(ty) = super::gpkg_blob::field_type(&c.decl) else {
                continue;
            };
            if ty == FieldType::Image {
                let (pictured, others) = super::gpkg::probe_pictures(&conn, &table, &c.name);
                if pictured == 0 && others > 0 {
                    dropped.push(c.name.clone());
                    continue;
                }
                if others > 0 {
                    notes.push(super::mixed_pictures_note(others, pictured + others, &c.name));
                }
            }
            fields.push(Field::new(c.name.clone(), ty));
        }
        if !dropped.is_empty() {
            notes.push(super::not_pictures_note(&dropped));
        }

        let count = count(&conn, &table);
        let extent = match &geometry {
            Some(g) => {
                if let Some(n) = count.filter(|n| *n > SAMPLE as u64) {
                    notes.push(format!("its extent comes from the first {SAMPLE} features of {n}"));
                }
                extent(&conn, &table, g, &reproject)
            }
            None => Extent::EMPTY,
        };
        let geometry_type = detection.geometry.as_ref().map(|g| g.geometry_type).unwrap_or(GeometryType::None);
        let zm = detection.geometry.as_ref().is_some_and(|g| g.zm);
        let mut info = SourceInfo {
            name: table.clone(),
            format: "SQLite",
            geometry_type,
            fields,
            extent,
            count,
            crs: reproject.label(),
            indexed: false,
            notes,
            zm_dropped: false,
        };
        if zm {
            info.drop_zm(None);
        }
        let file = conn.path().filter(|p| !p.is_empty()).map(|p| (PathBuf::from(p), id));
        Ok(SqliteSource { info, conn: Mutex::new(Some(conn)), table, key, geometry, srid, zm, reproject, temp, file })
    }

    fn select_list(&self) -> String {
        let mut cols = vec![self.key.as_deref().map(gpkg_quote).unwrap_or_else(|| "rowid".into())];
        cols.extend(self.geometry.iter().map(|g| gpkg_quote(g)));
        cols.extend(self.info.fields.iter().map(|f| gpkg_quote(&f.name)));
        cols.join(", ")
    }

    /// Turns a row (id, geometry?, fields…) into a feature; `Skip` for one outside the request
    /// (or without geometry), `Bad` for one that can't be read.
    fn feature(&self, r: &rusqlite::Row<'_>, native: Option<&Extent>, req: &Request) -> rusqlite::Result<Row> {
        // Integer keys, negative ones too, map to distinct ids.
        let Some(id) = r.get::<_, Option<i64>>(0)?.map(|i| i as u64) else { return Ok(Row::Bad) };
        let (geometry, at) = match &self.geometry {
            Some(_) => {
                let blob = match r.get_ref(1)? {
                    ValueRef::Blob(b) => b,
                    ValueRef::Null => return Ok(Row::Skip),
                    _ => return Ok(Row::Bad),
                };
                if sqlite_blob::is_empty(blob) {
                    return Ok(Row::Skip);
                }
                let Some((_, envelope, body)) = sqlite_blob::split_blob(blob) else { return Ok(Row::Bad) };
                if let (Some(q), Some(e)) = (native, envelope)
                    && !e.intersects(q)
                {
                    return Ok(Row::Skip);
                }
                let Some(g) = wkb::decode(body.as_ref()).and_then(|g| self.reproject.geometry(g)) else { return Ok(Row::Bad) };
                if !in_request(&g, req) {
                    return Ok(Row::Skip);
                }
                (Some(g), 2)
            }
            None => (None, 1),
        };
        let attrs = self
            .info
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| r.get_ref(i + at).map(|v| gpkg_value(v, f.ty)).unwrap_or(Value::Null))
            .collect();
        Ok(Row::Feature(Feature { id, geometry, attrs }))
    }
}

enum Row {
    Feature(Feature),
    /// Outside the request, or without geometry.
    Skip,
    /// Unreadable.
    Bad,
}

impl FeatureSource for SqliteSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let guard = self.conn.lock().map_err(|_| DataError::Format("SQLite connection poisoned".into()))?;
        let conn = guard.as_ref().ok_or_else(|| DataError::Format("SQLite connection closed".into()))?;
        let native = match (&self.geometry, req.extent) {
            (Some(_), Some(e)) => Some(self.reproject.extent_from_lonlat(&e)),
            _ => None,
        };
        let sql = format!("SELECT {} FROM {}", self.select_list(), gpkg_quote(&self.table));
        let mut st = conn.prepare(&sql).map_err(err)?;
        let mut rows = st.query([]).map_err(err)?;
        let mut out = Batch::default();
        let mut sample = if native.is_none() { Sampler::with_total(limit, self.info.count.unwrap_or(0)) } else { Sampler::new(limit) };
        while let Some(r) = rows.next().map_err(err)? {
            // With an extent, the envelope in the geometry's header says whether the row matches
            // before anything is decoded; without one, only the decoded geometry can, so the
            // match is counted after it.
            let envelope = match (&native, &self.geometry) {
                (Some(_), Some(_)) => match r.get_ref(1) {
                    Ok(ValueRef::Blob(b)) => sqlite_blob::split_blob(b).and_then(|(_, e, _)| e),
                    _ => None,
                },
                _ => None,
            };
            if let (Some(q), Some(e)) = (&native, envelope)
                && !e.intersects(q)
            {
                continue;
            }
            let known = native.is_none() || envelope.is_some();
            if known && !sample.wants() {
                continue;
            }
            match self.feature(r, native.as_ref(), req).map_err(err)? {
                Row::Feature(f) if known || sample.wants() => sample.keep(f),
                Row::Feature(_) | Row::Skip => {}
                Row::Bad => out.skipped += 1,
            }
        }
        out.matched = sample.seen();
        out.complete = sample.complete();
        out.features = sample.items;
        Ok(out)
    }

    /// Every table is scanned in full, whatever its index.
    fn visits_chunks(&self) -> bool {
        self.file.is_some()
    }

    /// A scan in table order, on a read-only connection of its own: one statement and so one
    /// snapshot of the database. A file put in place of the one opened is refused (where the
    /// system says files' identities); a file removed fails as missing.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let Some((path, id)) = &self.file else { return Err(DataError::Format("SQLite: no file to read".into())) };
        let same_file = || {
            std::fs::metadata(path)?;
            if id.is_some() && file_identity(path) != *id {
                return Err(DataError::Format(format!("{}: the file was replaced since it was added; add it again", path.display())));
            }
            Ok(())
        };
        same_file()?;
        let conn = open_db(path)?;
        same_file()?;
        let req = Request { extent, limit: usize::MAX };
        let native = match (&self.geometry, extent) {
            (Some(_), Some(e)) => Some(self.reproject.extent_from_lonlat(&e)),
            _ => None,
        };
        let sql = format!("SELECT {} FROM {}", self.select_list(), gpkg_quote(&self.table));
        let mut st = conn.prepare(&sql).map_err(err)?;
        let mut rows = st.query([]).map_err(err)?;
        let mut out = Batch { complete: true, ..Batch::default() };
        while let Some(r) = rows.next().map_err(err)? {
            match self.feature(r, native.as_ref(), &req).map_err(err)? {
                Row::Feature(f) => {
                    out.matched += 1;
                    chunks.push(f)?;
                }
                Row::Skip => {}
                Row::Bad => out.skipped += 1,
            }
        }
        chunks.flush()?;
        Ok(out)
    }

    /// The original bytes of one row's image column, read from the file on demand.
    fn image(&self, fid: u64, field: &str) -> Result<Vec<u8>> {
        if !self.info.fields.iter().any(|f| f.name == field && f.ty == FieldType::Image) {
            return Err(DataError::Unsupported(format!("{field} is not an image field of {}", self.info.name)));
        }
        let guard = self.conn.lock().map_err(|_| DataError::Format("SQLite connection poisoned".into()))?;
        let conn = guard.as_ref().ok_or_else(|| DataError::Format("SQLite connection closed".into()))?;
        let key = self.key.as_deref().map(gpkg_quote).unwrap_or_else(|| "rowid".into());
        let sql = format!("SELECT {f} FROM {t} WHERE {key} = ?1", f = gpkg_quote(field), t = gpkg_quote(&self.table));
        let mut st = conn.prepare(&sql).map_err(err)?;
        let mut rows = st.query([fid as i64]).map_err(err)?;
        let Some(r) = rows.next().map_err(err)? else {
            return Err(DataError::Format(format!("no row with id {fid}")));
        };
        match r.get_ref(0).map_err(err)? {
            ValueRef::Blob(b) if !b.is_empty() => Ok(b.to_vec()),
            _ => Err(DataError::Format(format!("the image field {field} of row {fid} is empty"))),
        }
    }
}

/// The table's columns, in declaration order, and its `INTEGER PRIMARY KEY` column (the rowid).
fn columns(conn: &Connection, table: &str) -> Result<(Vec<ColumnDecl>, Option<String>)> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({})", gpkg_quote(table))).map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                ColumnDecl { name: r.get::<_, String>(1)?, decl: r.get::<_, Option<String>>(2)?.unwrap_or_default() },
                r.get::<_, i64>(5)? == 1,
            ))
        })
        .map_err(err)?;
    let mut cols = Vec::new();
    let mut key = None;
    for (c, pk) in rows.filter_map(std::result::Result::ok) {
        if pk && c.decl.to_ascii_uppercase().contains("INT") {
            key = Some(c.name.clone());
        }
        cols.push(c);
    }
    Ok((cols, key))
}

/// Whether the table is `WITHOUT ROWID` (its rows stored in primary-key order, with no ids).
fn without_rowid(conn: &Connection, table: &str) -> Result<bool> {
    let sql: String = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND lower(name) = lower(?1)", [table], |r| {
            r.get::<_, Option<String>>(0)
        })
        .map_err(err)?
        .unwrap_or_default();
    Ok(sql.to_ascii_uppercase().contains("WITHOUT ROWID"))
}

/// Geometry a GeoPackage's metadata declares for the table (a GeoPackage named `.sqlite`).
fn gpkg_named(conn: &Connection, table: &str) -> Option<Named> {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'gpkg_geometry_columns'", [], |r| r.get::<_, i64>(0))
        .ok()?;
    let (column, srs_id, geometry_type): (String, Option<i64>, Option<String>) = conn
        .query_row("SELECT column_name, srs_id, geometry_type_name FROM gpkg_geometry_columns WHERE table_name = ?1", [table], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .ok()?;
    Some(Named { column, srid: srs_id.and_then(|s| i32::try_from(s).ok()), geometry_type })
}

/// Geometry a SpatiaLite's `geometry_columns` declares for the table.
fn spatialite_named(conn: &Connection, table: &str) -> Option<Named> {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND lower(name) = 'geometry_columns'", [], |r| r.get::<_, i64>(0))
        .ok()?;
    let (column, srid, geometry_type): (String, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT f_geometry_column, srid, geometry_type FROM geometry_columns WHERE lower(f_table_name) = lower(?1)",
            [table],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()?;
    Some(Named { column, srid: srid.and_then(|s| i32::try_from(s).ok()), geometry_type: geometry_type.map(|t| t.to_string()) })
}

/// The EPSG code an SRID means, from a SpatiaLite `spatial_ref_sys` when it has one; a positive
/// SRID with no such row is taken as an EPSG code, as writers usually mean by it.
fn epsg_of(conn: &Connection, srid: i32, notes: &mut Vec<String>) -> Option<u32> {
    let row: Option<(Option<String>, Option<i64>)> =
        conn.query_row("SELECT auth_name, auth_srid FROM spatial_ref_sys WHERE srid = ?1", [srid], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    match row {
        Some((Some(org), Some(id))) if org.eq_ignore_ascii_case("epsg") => u32::try_from(id).ok(),
        Some(_) => {
            notes.push(format!("its coordinate system (SRID {srid}) has no EPSG code; read as longitude/latitude"));
            None
        }
        None => u32::try_from(srid).ok(),
    }
}

/// A column's first few non-NULL values, for geometry detection.
fn sample_blobs(conn: &Connection, table: &str, column: &str) -> Vec<Vec<u8>> {
    let sql = format!("SELECT {c} FROM {t} WHERE {c} IS NOT NULL LIMIT 16", c = gpkg_quote(column), t = gpkg_quote(table));
    let Ok(mut st) = conn.prepare(&sql) else { return Vec::new() };
    let Ok(rows) = st.query_map([], |r| {
        Ok(match r.get_ref(0)? {
            ValueRef::Blob(b) if !b.is_empty() => Some(b.to_vec()),
            _ => None,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(std::result::Result::ok).flatten().collect()
}

/// The number of rows the table has.
fn count(conn: &Connection, table: &str) -> Option<u64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", gpkg_quote(table)), [], |r| r.get::<_, i64>(0))
        .ok()
        .and_then(|n| u64::try_from(n).ok())
}

/// The table's extent in longitude/latitude, from the first [`SAMPLE`] geometries.
fn extent(conn: &Connection, table: &str, column: &str, reproject: &Reproject) -> Extent {
    let sql = format!("SELECT {c} FROM {t} WHERE {c} IS NOT NULL LIMIT {SAMPLE}", c = gpkg_quote(column), t = gpkg_quote(table));
    let Ok(mut st) = conn.prepare(&sql) else { return Extent::EMPTY };
    let Ok(mut rows) = st.query([]) else { return Extent::EMPTY };
    let mut e = Extent::EMPTY;
    while let Ok(Some(r)) = rows.next() {
        let Ok(ValueRef::Blob(b)) = r.get_ref(0) else { continue };
        if let Some((_, envelope, body)) = sqlite_blob::split_blob(b) {
            match envelope {
                Some(x) => e = e.union(&x),
                None => {
                    if let Some(g) = wkb::decode(body.as_ref()) {
                        e = e.union(&g.extent());
                    }
                }
            }
        }
    }
    reproject.extent_to_lonlat(&e)
}

/// A file's device and inode, which tell it from another file put at its path since; `None`
/// where the system doesn't say them.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
