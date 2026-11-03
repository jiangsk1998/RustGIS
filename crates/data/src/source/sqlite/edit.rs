//! Whole-table editing of an ordinary SQLite table, with optimistic row versions and atomic saves.
//!
//! Unlike a GeoPackage there is no metadata table saying what changed: a revision is the database
//! file's size and modification time (and its journal's), which a stale project compares against
//! before inserting. Updates and deletes are checked row by row against the fingerprints read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::config::DbConfig;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params_from_iter};
use rustgis_geom::Extent;

use super::{SqliteSource, err, gpkg_named, gpkg_quote as quote, sample_blobs, spatialite_named, without_rowid};
use crate::raster::Crs;
use crate::source::SourceInfo;
use crate::source::edits::{Conflict, Edits, Saved};
use crate::source::gpkg::{sql_value, value};
use crate::source::sqlite_blob::{self, GeometryFormat, Shape};
use crate::{DataError, Feature, Field, FieldType, Geometry, GeometryType, Result, Value, wkb};

/// A whole-table editing session is bounded, unlike a streamed map source.
pub const MAX_EDITABLE: usize = 100_000;

/// Rows fetched or tested at a time.
const CHUNK: usize = 500;

/// Rows loaded for editing, including geometry-less rows but excluding unreadable geometries.
pub struct Editable {
    pub features: Vec<Feature>,
    pub versions: Vec<(u64, i64)>,
    /// The database revision from the same read as the rows.
    pub revision: String,
    pub matched: u64,
    pub skipped: u64,
    pub writable: bool,
}

/// Stored row versions and snapshots, read in the write transaction before it commits.
#[derive(Debug)]
pub struct SavedRows {
    pub changes: Saved,
    /// Requested snapshots, followed by final snapshots of trigger-affected resident keys.
    pub features: Vec<Feature>,
    /// Affected known resident keys absent after triggers, in unsigned key order.
    pub removed: Vec<u64>,
    /// Supplied field positions that have no stored column.
    pub unstored_fields: Vec<usize>,
    /// Image fields, which stay in the file and are never written.
    pub read_only: Vec<String>,
    /// Whether a trigger changed rows beyond the submitted edits.
    pub triggers: bool,
    pub revision: String,
}

/// How the table stored its geometry when it was opened.
#[derive(Clone, Copy, Debug)]
struct Storage {
    format: GeometryFormat,
    srid: Option<i32>,
    shape: Shape,
    zm: bool,
}

/// An existing feature table; its read connection is never reused for writes.
pub struct SqliteEditTable {
    src: SqliteSource,
    path: PathBuf,
    columns: Vec<String>,
    key: String,
    geometry: Option<String>,
    storage: Option<Storage>,
}

impl SqliteEditTable {
    pub fn open(path: &Path, table: &str) -> Result<Self> {
        let src = SqliteSource::open(path, Some(table))?;
        if src.temp.is_some() {
            return Err(DataError::Format(format!(
                "{}: a database opened from memory can be read but not saved; open it from its file",
                path.display()
            )));
        }
        let geometry = src.geometry.clone();
        let (columns, key) = {
            let guard = src.conn.lock().map_err(|_| DataError::Format("SQLite connection poisoned".into()))?;
            let conn = guard.as_ref().ok_or_else(|| DataError::Format("SQLite connection closed".into()))?;
            let cols = table_columns(conn, &src.table)?;
            let mut primary = cols.iter().filter(|c| c.pk > 0);
            let key = match primary.next() {
                Some(c) if primary.next().is_none() => {
                    if !c.decl.to_ascii_uppercase().contains("INT") {
                        return Err(DataError::Format(format!(
                            "{}: its primary key {} isn't an INTEGER, so its rows can't be told apart to save",
                            src.table, c.name
                        )));
                    }
                    c.name.clone()
                }
                Some(_) => {
                    return Err(DataError::Format(format!(
                        "{}: it has a composite primary key, so its rows can't be told apart to save",
                        src.table
                    )));
                }
                None => {
                    if without_rowid(conn, &src.table)? {
                        return Err(DataError::Format(format!("{}: it is WITHOUT ROWID, so its rows have no id", src.table)));
                    }
                    "rowid".to_string()
                }
            };
            (cols.into_iter().map(|c| c.name).collect::<Vec<_>>(), key)
        };
        let storage = match &geometry {
            Some(column) => {
                let guard = src.conn.lock().map_err(|_| DataError::Format("SQLite connection poisoned".into()))?;
                let conn = guard.as_ref().ok_or_else(|| DataError::Format("SQLite connection closed".into()))?;
                Some(detect_storage(conn, &src.table, column, &src, None))
            }
            None => None,
        };
        Ok(Self { src, path: path.to_path_buf(), columns, key, geometry, storage })
    }

    pub fn info(&self) -> &SourceInfo {
        &self.src.info
    }

    pub fn crs(&self) -> Option<Crs> {
        (!self.src.reproject.is_lonlat()).then_some(self.src.reproject.crs)
    }

    pub fn table(&self) -> &str {
        &self.src.table
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn geometry_column(&self) -> Option<&str> {
        self.geometry.as_deref()
    }

    /// Its geometry storage (`gpkg`, `spatialite` or `wkb`), or `wkb` for a table without geometry.
    pub fn format_name(&self) -> &'static str {
        match self.storage.map(|s| s.format) {
            Some(GeometryFormat::Gpkg) => "gpkg",
            Some(GeometryFormat::SpatiaLite) => "spatialite",
            _ => "wkb",
        }
    }

    /// The stored shape (single or multi), or `point` for a table without geometry.
    pub fn shape_name(&self) -> &'static str {
        self.storage.map(|s| s.shape.name()).unwrap_or("point")
    }

    /// The SRID its geometry declares (0 when unknown, read as longitude/latitude).
    pub fn srid(&self) -> i32 {
        self.storage.and_then(|s| s.srid).unwrap_or(0)
    }

    /// Whether its stored geometry carries Z or M values.
    pub fn zm(&self) -> bool {
        self.storage.is_some_and(|s| s.zm)
    }

    pub fn read_editable(&self, limit: usize) -> Result<Editable> {
        let guard = self.src.conn.lock().map_err(|_| DataError::Format("SQLite connection poisoned".into()))?;
        let conn = guard.as_ref().ok_or_else(|| DataError::Format("SQLite connection closed".into()))?;
        let tx = conn.unchecked_transaction().map_err(err)?;
        let matched: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM {}", quote(self.table())), [], |r| r.get(0)).map_err(err)?;
        let matched = u64::try_from(matched).map_err(|_| DataError::Format("SQLite: negative row count".into()))?;
        let geom = match self.geometry.as_deref() {
            Some(g) => Some(self.columns.iter().position(|c| c.eq_ignore_ascii_case(g)).ok_or_else(|| self.shape_error())?),
            None => None,
        };
        let attrs = self
            .info()
            .fields
            .iter()
            .map(|f| self.columns.iter().position(|c| c.eq_ignore_ascii_case(&f.name)).ok_or_else(|| self.shape_error()))
            .collect::<Result<Vec<_>>>()?;
        let mut out = Editable {
            features: Vec::new(),
            versions: Vec::new(),
            revision: file_revision(&self.path)?,
            matched,
            skipped: 0,
            writable: std::fs::metadata(&self.path).is_ok_and(|m| !m.permissions().readonly()),
        };
        {
            let mut st = tx
                .prepare(&format!(
                    "SELECT {} FROM {} ORDER BY {} LIMIT ?1",
                    select_list(&self.columns, &self.key),
                    quote(self.table()),
                    key_sql(&self.key)
                ))
                .map_err(err)?;
            let mut rows = st.query([limit.clamp(1, MAX_EDITABLE) as i64]).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                let id = row.get::<_, i64>(0).map_err(err)?.cast_unsigned();
                let geometry = match geom {
                    Some(i) => match self.geometry(row.get_ref(i + 1).map_err(err)?) {
                        Some(g) => g,
                        None => {
                            out.skipped += 1;
                            continue;
                        }
                    },
                    None => None,
                };
                let mut hash = FNV_OFFSET;
                for i in 0..=self.columns.len() {
                    hash_value(&mut hash, row.get_ref(i).map_err(err)?);
                }
                let attrs = self
                    .info()
                    .fields
                    .iter()
                    .zip(&attrs)
                    .map(|(f, &i)| row.get_ref(i + 1).map(|v| value(v, f.ty)))
                    .collect::<rusqlite::Result<_>>()
                    .map_err(err)?;
                out.versions.push((id, hash.cast_signed()));
                out.features.push(Feature { id, geometry, attrs });
            }
        }
        tx.commit().map_err(err)?;
        Ok(out)
    }

    /// Returns conflicts without changing anything; every other failure also rolls back the save.
    /// `reserved` must include every loaded baseline key, including rows not changed by this save.
    /// Pending inserts also require `expected_revision` to match the database file's revision.
    pub fn save(
        &self,
        edits: &Edits,
        fields: &[Field],
        reserved: impl Iterator<Item = u64>,
        expected_revision: &str,
    ) -> Result<std::result::Result<SavedRows, Vec<Conflict>>> {
        let mut conn =
            Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(err)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(err)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false).map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        register_functions(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(err)?;
        if !edits.inserts.is_empty() {
            let revision = file_revision(&self.path)?;
            if revision != expected_revision {
                return Err(DataError::Format(format!(
                    "{}: the database changed since it was loaded; refusing stale inserts, so nothing was saved. Discard Edits loads it again",
                    self.table()
                )));
            }
        }
        let current_columns = table_columns(&tx, self.table())?;
        if current_columns.len() != self.columns.len()
            || !current_columns.iter().zip(&self.columns).all(|(c, name)| c.name.eq_ignore_ascii_case(name))
        {
            return Err(self.shape_error());
        }
        let names: Vec<String> = current_columns.iter().map(|c| c.name.clone()).collect();
        let (key_pos, geom_pos) = positions_of(&names, &self.key, self.geometry.as_deref()).ok_or_else(|| self.shape_error())?;
        let key_name = match key_pos {
            Some(i) => names.get(i).ok_or_else(|| self.shape_error())?.clone(),
            None => "rowid".to_string(),
        };
        let geom_name = geom_pos.and_then(|i| names.get(i).cloned());
        match (&geom_name, self.storage) {
            (Some(column), Some(loaded)) => {
                let now = detect_storage(&tx, self.table(), column, &self.src, Some(loaded));
                if now.format != loaded.format || now.srid != loaded.srid || now.shape != loaded.shape || now.zm != loaded.zm {
                    return Err(self.shape_error());
                }
            }
            (None, None) => {}
            _ => return Err(self.shape_error()),
        }

        let residents: HashMap<i64, bool> = reserved
            .map(u64::cast_signed)
            .chain(edits.updates.iter().map(|(f, _)| f.id.cast_signed()))
            .chain(edits.deletes.iter().map(|(k, _)| k.cast_signed()))
            .map(|k| (k, false))
            .collect();
        let reserved_high_water = if edits.inserts.is_empty() { 0 } else { residents.keys().copied().max().unwrap_or(0).max(0) };
        let absent_before = self.missing_keys(&tx, &residents.keys().copied().collect::<Vec<_>>())?;
        let feature_writes = watch_feature_writes(&tx, self.table(), &key_name, residents)?;
        let mut direct_changes = 0_u64;
        let mut saved = Saved::default();
        let mut read_only = Vec::new();
        let mut targets = Vec::new();
        let mut snapshot_columns = Vec::new();
        let mut unstored_fields = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            if let Some(c) = names.iter().position(|c| c.eq_ignore_ascii_case(&f.name)) {
                snapshot_columns.push((i, c));
                if Some(c) == key_pos || Some(c) == geom_pos {
                    saved.skipped_fields.push(f.name.clone());
                } else if f.ty == FieldType::Image {
                    read_only.push(f.name.clone());
                    unstored_fields.push(i);
                } else {
                    targets.push((i, c));
                }
            } else {
                saved.skipped_fields.push(f.name.clone());
                unstored_fields.push(i);
            }
        }
        let keys: Vec<i64> =
            edits.updates.iter().map(|(f, _)| f.id.cast_signed()).chain(edits.deletes.iter().map(|(k, _)| k.cast_signed())).collect();
        let current = self.fetch_rows(&tx, &names, &keys)?;
        let mut conflicts = Vec::new();
        for (k, version) in edits.updates.iter().map(|(f, v)| (f.id, *v)).chain(edits.deletes.iter().copied()) {
            match current.get(&k.cast_signed()) {
                None => conflicts.push(Conflict::Deleted(k)),
                Some(row) if fingerprint(row.iter().map(ValueRef::from)) != version => conflicts.push(Conflict::Changed(k)),
                Some(_) => {}
            }
        }
        if !conflicts.is_empty() {
            return Ok(Err(conflicts));
        }

        let table = quote(self.table());
        let pk = key_sql(&self.key);
        for (f, _) in &edits.updates {
            let row = current.get(&f.id.cast_signed()).ok_or_else(|| self.shape_error())?;
            let mut sets = Vec::new();
            let mut args = Vec::new();
            for &(i, c) in &targets {
                let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                let old = row.get(c + 1).ok_or_else(|| self.shape_error())?;
                let attr = f.attrs.get(i).unwrap_or(&Value::Null);
                if value(ValueRef::from(old), field.ty) != *attr {
                    sets.push(format!("{} = ?", quote(names.get(c).ok_or_else(|| self.shape_error())?)));
                    args.push(sql_value(attr, field.ty));
                }
            }
            if let Some(geom) = geom_pos {
                let old = row.get(geom + 1).ok_or_else(|| self.shape_error())?;
                let geometry_changed = self.geometry(ValueRef::from(old)).ok_or_else(|| self.shape_error())? != f.geometry;
                if geometry_changed {
                    let blob = self.geometry_blob(f.geometry.as_ref(), Some((f.id, ValueRef::from(old))))?;
                    sets.push(format!("{} = ?", quote(geom_name.as_deref().ok_or_else(|| self.shape_error())?)));
                    args.push(blob);
                }
            }
            if sets.is_empty() {
                saved.unchanged += 1;
                continue;
            }
            args.push(Sql::Integer(f.id.cast_signed()));
            direct_changes +=
                tx.execute(&format!("UPDATE {table} SET {} WHERE {pk} = ?", sets.join(", ")), params_from_iter(args)).map_err(err)? as u64;
            saved.updated += 1;
        }
        drop(current);
        let mut high_water = 0;
        if !edits.inserts.is_empty() {
            high_water = reserved_high_water;
            let current_max: Option<i64> = tx.query_row(&format!("SELECT max({pk}) FROM {table}"), [], |r| r.get(0)).map_err(err)?;
            high_water = high_water.max(current_max.unwrap_or(0));
            let has_sequence: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence')", [], |r| r.get(0))
                .map_err(err)?;
            if has_sequence {
                let sequence: Option<i64> = tx
                    .query_row("SELECT max(seq) FROM sqlite_sequence WHERE lower(name) = lower(?1)", [self.table()], |r| r.get(0))
                    .map_err(err)?;
                high_water = high_water.max(sequence.unwrap_or(0));
            }
        }
        for f in &edits.inserts {
            let new_key = high_water.checked_add(1).ok_or_else(|| {
                DataError::Format(format!("{}: no positive INTEGER PRIMARY KEY remains above the reserved keys", self.table()))
            })?;
            high_water = new_key;
            let mut names_sql = vec![pk.clone()];
            let mut args = vec![Sql::Integer(new_key)];
            if let Some(g) = &f.geometry {
                let blob = self.geometry_blob(Some(g), None)?;
                names_sql.push(quote(geom_name.as_deref().ok_or_else(|| self.shape_error())?));
                args.push(blob);
            }
            for &(i, c) in &targets {
                let attr = f.attrs.get(i).unwrap_or(&Value::Null);
                if *attr != Value::Null {
                    let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                    names_sql.push(quote(names.get(c).ok_or_else(|| self.shape_error())?));
                    args.push(sql_value(attr, field.ty));
                }
            }
            let sql = format!("INSERT INTO {table} ({}) VALUES ({})", names_sql.join(", "), vec!["?"; names_sql.len()].join(", "));
            direct_changes += tx.execute(&sql, params_from_iter(args)).map_err(err)? as u64;
            saved.inserted.push((f.id, new_key.cast_unsigned()));
        }
        for (k, _) in &edits.deletes {
            direct_changes += tx.execute(&format!("DELETE FROM {table} WHERE {pk} = ?1"), [k.cast_signed()]).map_err(err)? as u64;
        }
        feature_writes.allowed.store(false, Ordering::Relaxed);
        let affected = feature_writes.affected()?;
        let unmarked: Vec<i64> = feature_writes.unmarked()?.into_iter().filter(|k| absent_before.binary_search(k).is_err()).collect();
        let evicted = self.missing_keys(&tx, &unmarked)?;
        let mut keys: Vec<i64> = edits
            .updates
            .iter()
            .map(|(f, _)| f.id.cast_signed())
            .chain(saved.inserted.iter().map(|(_, k)| k.cast_signed()))
            .chain(edits.deletes.iter().map(|(k, _)| k.cast_signed()))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        let additional: Vec<i64> = affected.iter().copied().filter(|k| keys.binary_search(k).is_err()).collect();
        keys.extend(&additional);
        let rows = self.fetch_rows(&tx, &names, &keys)?;
        let mut removed: Vec<u64> = affected.iter().filter(|k| !rows.contains_key(*k)).chain(&evicted).map(|k| k.cast_unsigned()).collect();
        removed.sort_unstable();
        removed.dedup();
        saved.deleted = edits.deletes.iter().filter(|(k, _)| !rows.contains_key(&k.cast_signed())).count();
        let triggers = feature_writes.count.load(Ordering::Relaxed) != direct_changes || !evicted.is_empty();
        let mut features = Vec::with_capacity(keys.len());
        let originals = edits
            .updates
            .iter()
            .map(|(f, _)| (Some(f), f.id.cast_signed()))
            .chain(edits.inserts.iter().zip(&saved.inserted).map(|(f, (_, k))| (Some(f), k.cast_signed())))
            .chain(edits.deletes.iter().filter(|(k, _)| rows.contains_key(&k.cast_signed())).map(|(k, _)| (None, k.cast_signed())))
            .chain(additional.iter().filter(|k| rows.contains_key(*k)).map(|k| (None, *k)));
        for (original, k) in originals {
            let row = rows.get(&k).ok_or_else(|| DataError::Format(format!("row {k}: it was removed while saving")))?;
            let geometry = match geom_pos {
                Some(i) => self
                    .geometry(ValueRef::from(row.get(i + 1).ok_or_else(|| self.shape_error())?))
                    .ok_or_else(|| DataError::Format(format!("row {k}: its stored geometry can't be read, so nothing was saved")))?,
                None => None,
            };
            let mut feature = Feature { id: k.cast_unsigned(), geometry, attrs: vec![Value::Null; fields.len()] };
            if let Some(original) = original {
                for &i in &unstored_fields {
                    if let (Some(target), Some(submitted)) = (feature.attrs.get_mut(i), original.attrs.get(i)) {
                        target.clone_from(submitted);
                    }
                }
            }
            for &(i, c) in &snapshot_columns {
                let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                let attr = feature.attrs.get_mut(i).ok_or_else(|| self.shape_error())?;
                *attr = value(ValueRef::from(row.get(c + 1).ok_or_else(|| self.shape_error())?), field.ty);
            }
            saved.versions.push((k.cast_unsigned(), fingerprint(row.iter().map(ValueRef::from))));
            features.push(feature);
        }
        tx.commit().map_err(err)?;
        let revision = file_revision(&self.path)?;
        Ok(Ok(SavedRows { changes: saved, features, removed, unstored_fields, read_only, triggers, revision }))
    }

    fn shape_error(&self) -> DataError {
        DataError::Format(format!("{} changed shape since it was loaded; Discard Edits loads it again", self.table()))
    }

    // Outer None is unreadable; inner None is a valid NULL or empty geometry.
    fn geometry(&self, v: ValueRef<'_>) -> Option<Option<Geometry>> {
        match v {
            ValueRef::Null => Some(None),
            ValueRef::Blob(b) if sqlite_blob::is_empty(b) => Some(None),
            ValueRef::Blob(b) => {
                let (_, _, body) = sqlite_blob::split_blob(b)?;
                self.src.reproject.geometry(wkb::decode(body.as_ref())?).map(Some)
            }
            _ => None,
        }
    }

    fn geometry_blob(&self, g: Option<&Geometry>, row: Option<(u64, ValueRef<'_>)>) -> Result<Sql> {
        let Some(loaded) = self.storage else {
            return Err(DataError::Format(format!("{} has no geometry column", self.table())));
        };
        let old = row.and_then(|(_, v)| match v {
            ValueRef::Blob(b) => sqlite_blob::split_blob(b),
            _ => None,
        });
        let has_zm = old.as_ref().is_some_and(|(_, _, body)| wkb::has_zm(body.as_ref()));
        if loaded.zm || has_zm {
            let message = match row {
                Some((k, _)) => format!(
                    "row {}: its geometry has Z or M values, which RustGIS doesn't keep, so its geometry can't be saved (its attributes can)",
                    k.cast_signed()
                ),
                None => format!(
                    "{} needs Z or M values, which RustGIS doesn't keep, so new features with a geometry can't be saved",
                    self.table()
                ),
            };
            return Err(DataError::Format(message));
        }
        let Some(g) = g else {
            return Ok(Sql::Null);
        };
        let native = self.src.reproject.to_native(g).ok_or_else(|| {
            let id = row.map(|(k, _)| k.cast_signed().to_string()).unwrap_or_else(|| "new".into());
            DataError::Format(format!("row {id}: its geometry can't be converted to {}", self.src.reproject.label()))
        })?;
        let format = old.as_ref().map(|(f, _, _)| *f).unwrap_or(loaded.format);
        let shape = old.as_ref().and_then(|(_, _, body)| Shape::of(body.as_ref())).unwrap_or(loaded.shape);
        let body = encode_shape(&native, shape).ok_or_else(|| {
            DataError::Format(format!("{}'s {} column can't hold this {:?} geometry", self.table(), shape.name(), native.kind()))
        })?;
        let extent = native.extent();
        let srid = loaded.srid.unwrap_or(0);
        let blob = match format {
            GeometryFormat::Gpkg => sqlite_blob::gpkg_blob(&body, srid, &extent),
            GeometryFormat::SpatiaLite => sqlite_blob::spatialite_blob(&body, srid, &extent).ok_or_else(|| self.shape_error())?,
            GeometryFormat::Wkb => {
                if srid > 0 {
                    sqlite_blob::ewkb(&body, srid).ok_or_else(|| self.shape_error())?
                } else {
                    body
                }
            }
        };
        Ok(Sql::Blob(blob))
    }

    /// Keys among `keys` with no row now, read through the key column only.
    fn missing_keys(&self, conn: &Connection, keys: &[i64]) -> Result<Vec<i64>> {
        let mut present = HashSet::with_capacity(keys.len());
        let pk = key_sql(&self.key);
        let table = quote(self.table());
        for chunk in keys.chunks(CHUNK) {
            let mut st =
                conn.prepare(&format!("SELECT {pk} FROM {table} WHERE {pk} IN ({})", vec!["?"; chunk.len()].join(","))).map_err(err)?;
            let mut rows = st.query(params_from_iter(chunk.iter())).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                present.insert(row.get::<_, i64>(0).map_err(err)?);
            }
        }
        let mut missing: Vec<i64> = keys.iter().copied().filter(|k| !present.contains(k)).collect();
        missing.sort_unstable();
        Ok(missing)
    }

    /// The rows of `keys`, each as a vector of its key followed by its columns.
    fn fetch_rows(&self, conn: &Connection, columns: &[String], keys: &[i64]) -> Result<HashMap<i64, Vec<Sql>>> {
        let mut out = HashMap::new();
        let pk = key_sql(&self.key);
        let table = quote(self.table());
        for chunk in keys.chunks(CHUNK) {
            let mut st = conn
                .prepare(&format!(
                    "SELECT {} FROM {table} WHERE {pk} IN ({})",
                    select_list(columns, &self.key),
                    vec!["?"; chunk.len()].join(",")
                ))
                .map_err(err)?;
            let mut rows = st.query(params_from_iter(chunk.iter())).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                let k = row.get::<_, i64>(0).map_err(err)?;
                let vals = (0..=columns.len()).map(|i| row.get(i)).collect::<rusqlite::Result<Vec<Sql>>>().map_err(err)?;
                out.insert(k, vals);
            }
        }
        Ok(out)
    }
}

/// The table's columns, in declaration order, with their declared types and primary-key positions.
struct Column {
    name: String,
    decl: String,
    pk: i64,
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<Column>> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({})", quote(table))).map_err(err)?;
    st.query_map([], |r| Ok(Column { name: r.get(1)?, decl: r.get::<_, Option<String>>(2)?.unwrap_or_default(), pk: r.get(5)? }))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)
}

/// The key's column position (`None` when rows are keyed by rowid) and the geometry column's.
fn positions_of(columns: &[String], key: &str, geometry: Option<&str>) -> Option<(Option<usize>, Option<usize>)> {
    let key_pos = columns.iter().position(|c| c.eq_ignore_ascii_case(key));
    if key_pos.is_none() && !key.eq_ignore_ascii_case("rowid") {
        return None;
    }
    let geometry = match geometry {
        Some(g) => Some(columns.iter().position(|c| c.eq_ignore_ascii_case(g))?),
        None => None,
    };
    Some((key_pos, geometry))
}

/// The select list of every row: the key first, then the table's columns in declaration order.
fn select_list(columns: &[String], key: &str) -> String {
    let mut out = vec![key_sql(key)];
    out.extend(columns.iter().map(|c| quote(c)));
    out.join(", ")
}

/// The expression naming a row's key: its column, or SQLite's implicit rowid.
fn key_sql(key: &str) -> String {
    if key.eq_ignore_ascii_case("rowid") { "rowid".into() } else { quote(key) }
}

/// The table's geometry storage, from the first readable sampled geometry, else what it was
/// (`fallback`, when re-checking before a save), else its metadata.
fn detect_storage(conn: &Connection, table: &str, column: &str, src: &SqliteSource, fallback: Option<Storage>) -> Storage {
    for blob in sample_blobs(conn, table, column) {
        if sqlite_blob::is_empty(&blob) {
            continue;
        }
        let Some((format, _, body)) = sqlite_blob::split_blob(&blob) else { continue };
        let Some(shape) = Shape::of(body.as_ref()) else { continue };
        return Storage { format, srid: sqlite_blob::srid(&blob).or(src.srid), shape, zm: wkb::has_zm(body.as_ref()) };
    }
    if let Some(loaded) = fallback {
        return loaded;
    }
    let metadata =
        gpkg_named(conn, table).map(|_| GeometryFormat::Gpkg).or_else(|| spatialite_named(conn, table).map(|_| GeometryFormat::SpatiaLite));
    Storage {
        format: metadata.unwrap_or(GeometryFormat::Wkb),
        srid: src.srid,
        shape: match src.info.geometry_type {
            GeometryType::Point => Shape::Point,
            GeometryType::Polyline => Shape::Line,
            GeometryType::Polygon => Shape::Polygon,
            GeometryType::None => Shape::Point,
        },
        zm: src.zm,
    }
}

/// A geometry as the WKB body a column of `shape` holds; `None` when it doesn't fit.
fn encode_shape(g: &Geometry, shape: Shape) -> Option<Vec<u8>> {
    match (shape.is_multi(), shape, g) {
        (true, Shape::MultiPoint, Geometry::Point(c)) => Some(wkb::encode(&Geometry::MultiPoint(vec![*c]))),
        (true, Shape::MultiPoint, Geometry::MultiPoint(_))
        | (true, Shape::MultiLine, Geometry::Line(_))
        | (true, Shape::MultiPolygon, Geometry::Polygon(_)) => Some(wkb::encode(g)),
        (false, Shape::Point, Geometry::Point(_)) => Some(wkb::encode(g)),
        (false, Shape::Line, Geometry::Line(parts)) if parts.len() == 1 => wkb::encode_single(g),
        (false, Shape::Polygon, Geometry::Polygon(polys)) if polys.len() == 1 => wkb::encode_single(g),
        _ => None,
    }
}

/// A database file's revision: the size and modification time of the file and of its journal.
fn file_revision(path: &Path) -> Result<String> {
    let mut out = String::new();
    for (tag, mut p) in [("db", path.to_path_buf()), ("wal", path.to_path_buf()), ("journal", path.to_path_buf())] {
        if tag != "db" {
            let mut name = p.as_os_str().to_os_string();
            name.push(if tag == "wal" { "-wal" } else { "-journal" });
            p = PathBuf::from(name);
        }
        let m = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(e) if tag != "db" && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(DataError::Format(format!("{}: {e}", p.display()))),
        };
        let modified = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        out.push_str(&format!("{tag}:{}:{modified};", m.len()));
    }
    Ok(out)
}

struct FeatureWrites {
    count: AtomicU64,
    allowed: AtomicBool,
    residents: Mutex<HashMap<i64, bool>>,
}

impl FeatureWrites {
    fn affected(&self) -> Result<Vec<i64>> {
        let residents = self.residents.lock().map_err(|_| DataError::Format("SQLite write observer poisoned".into()))?;
        let mut keys: Vec<i64> = residents.iter().filter_map(|(&k, &changed)| changed.then_some(k)).collect();
        keys.sort_unstable();
        Ok(keys)
    }

    fn unmarked(&self) -> Result<Vec<i64>> {
        let residents = self.residents.lock().map_err(|_| DataError::Format("SQLite write observer poisoned".into()))?;
        Ok(residents.iter().filter_map(|(&k, &changed)| (!changed).then_some(k)).collect())
    }
}

fn watch_feature_writes(conn: &Connection, table: &str, key: &str, residents: HashMap<i64, bool>) -> Result<Arc<FeatureWrites>> {
    let state = Arc::new(FeatureWrites { count: AtomicU64::new(0), allowed: AtomicBool::new(true), residents: Mutex::new(residents) });
    let observed = state.clone();
    conn.create_scalar_function("rustgis_feature_write", 2, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS, move |ctx| {
        if !observed.allowed.load(Ordering::Relaxed) {
            return Err(rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(
                "triggers cannot change feature rows after the saved snapshot",
            ))));
        }
        observed.count.fetch_add(1, Ordering::Relaxed);
        let old = ctx.get::<Option<i64>>(0)?;
        let new = ctx.get::<Option<i64>>(1)?;
        let mut residents = observed
            .residents
            .lock()
            .map_err(|_| rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other("SQLite write observer poisoned"))))?;
        for key in old.into_iter().chain(new) {
            if let Some(changed) = residents.get_mut(&key) {
                *changed = true;
            }
        }
        Ok(0_i64)
    })
    .map_err(err)?;
    let table = quote(table);
    let key = key_sql(key);
    for (event, old, new) in [
        ("INSERT", "NULL".to_string(), format!("NEW.{key}")),
        ("UPDATE", format!("OLD.{key}"), format!("NEW.{key}")),
        ("DELETE", format!("OLD.{key}"), "NULL".to_string()),
    ] {
        conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER rustgis_observe_{event} AFTER {event} ON main.{table}
             BEGIN SELECT rustgis_feature_write({old}, {new}); END;"
        ))
        .map_err(err)?;
    }
    Ok(state)
}

/// Geometry functions a SpatiaLite table's own triggers call to keep its spatial index up to date.
fn register_functions(conn: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS;
    conn.create_scalar_function("ST_IsEmpty", 1, flags, |ctx| {
        Ok(match ctx.get_raw(0) {
            ValueRef::Blob(b) => Some(i64::from(sqlite_blob::is_empty(b))),
            _ => None,
        })
    })
    .map_err(err)?;
    for (name, index) in [("ST_MinX", 0), ("ST_MaxX", 1), ("ST_MinY", 2), ("ST_MaxY", 3)] {
        conn.create_scalar_function(name, 1, flags, move |ctx| {
            Ok(native_extent(ctx.get_raw(0)).map(|e| match index {
                0 => e.min_x,
                1 => e.max_x,
                2 => e.min_y,
                _ => e.max_y,
            }))
        })
        .map_err(err)?;
    }
    Ok(())
}

fn native_extent(v: ValueRef<'_>) -> Option<Extent> {
    let ValueRef::Blob(b) = v else { return None };
    if sqlite_blob::is_empty(b) {
        return None;
    }
    let (_, envelope, body) = sqlite_blob::split_blob(b)?;
    envelope.or_else(|| wkb::decode(body.as_ref()).map(|g| g.extent())).filter(|e| !e.is_empty())
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x100_0000_01b3;

fn fingerprint<'a>(vals: impl Iterator<Item = ValueRef<'a>>) -> i64 {
    let mut hash = FNV_OFFSET;
    for v in vals {
        hash_value(&mut hash, v);
    }
    hash.cast_signed()
}

fn hash_value(hash: &mut u64, v: ValueRef<'_>) {
    let mut bytes = |b: &[u8]| {
        for x in b {
            *hash = (*hash ^ u64::from(*x)).wrapping_mul(FNV_PRIME);
        }
    };
    match v {
        ValueRef::Null => bytes(&[0]),
        ValueRef::Integer(v) => {
            bytes(&[1]);
            bytes(&v.to_le_bytes());
        }
        ValueRef::Real(v) => {
            bytes(&[2]);
            bytes(&v.to_bits().to_le_bytes());
        }
        ValueRef::Text(v) => {
            bytes(&[3]);
            bytes(&(v.len() as u64).to_le_bytes());
            bytes(v);
        }
        ValueRef::Blob(v) => {
            bytes(&[4]);
            bytes(&(v.len() as u64).to_le_bytes());
            bytes(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_geom::Coord;

    fn line(x: f64) -> Geometry {
        Geometry::Line(vec![vec![Coord::new(x, 2.0), Coord::new(x + 1.0, 3.0)]])
    }

    fn picture() -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out).write_image(&[0u8; 3], 1, 1, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    fn plain(g: &Geometry) -> Vec<u8> {
        wkb::encode_single(g).expect("a single-part geometry")
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE places (id INTEGER PRIMARY KEY, name TEXT, lanes INTEGER, photo BLOB, geom BLOB);").unwrap();
        let g = line(1.0);
        conn.execute(
            "INSERT INTO places VALUES (1, 'one', 2, ?1, ?2)",
            rusqlite::params![picture(), sqlite_blob::gpkg_blob(&plain(&g), 4326, &g.extent())],
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn updates_inserts_and_deletes_round_trip() {
        let (_dir, path) = fixture();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(read.features.len(), 1);
        assert_eq!(table.format_name(), "gpkg");
        assert_eq!(table.shape_name(), "line");
        assert_eq!(table.srid(), 4326);
        assert_eq!(table.geometry_column(), Some("geom"));

        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("two".into());
        changed.geometry = Some(line(5.0));
        let inserted = Feature { id: 100, geometry: Some(line(9.0)), attrs: vec![Value::Text("new".into()), Value::Int(3), Value::Null] };
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], inserts: vec![inserted], ..Default::default() };
        let saved = table.save(&edits, &fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!((saved.changes.updated, saved.changes.unchanged), (1, 0));
        assert_eq!(saved.changes.inserted, vec![(100, 2)]);
        assert_eq!(saved.read_only, vec!["photo".to_string()]);
        assert!(saved.unstored_fields.contains(&2));
        assert!(!saved.triggers);
        let conn = Connection::open(&path).unwrap();
        let (name, lanes, photo): (String, i64, Vec<u8>) =
            conn.query_row("SELECT name, lanes, photo FROM places WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((name.as_str(), lanes), ("two", 2));
        assert_eq!(photo, picture());
        let mut stored = conn
            .prepare("SELECT id, geom FROM places ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        stored.sort_by_key(|(k, _)| *k);
        assert_eq!(stored.len(), 2);
        for ((key, blob), expected) in stored.iter().zip([line(5.0), line(9.0)]) {
            let (format, _, body) = sqlite_blob::split_blob(blob).expect("stored geometry");
            assert_eq!(format, GeometryFormat::Gpkg);
            assert_eq!(wkb::decode(body.as_ref()), Some(expected), "row {key}");
        }
        assert_eq!(saved.changes.versions, table.read_editable(MAX_EDITABLE).unwrap().versions);

        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let edits = Edits { deletes: vec![(1, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.deleted, 1);
        assert_eq!(saved.removed, vec![1]);
        assert_eq!(conn.query_row("SELECT count(*) FROM places", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT id FROM places", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn conflicts_are_reported_without_writing() {
        let (_dir, path) = fixture();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("mine".into());
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], ..Default::default() };
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE places SET name = 'theirs' WHERE id = 1", []).unwrap();
        assert_eq!(table.save(&edits, &fields, std::iter::empty(), &read.revision).unwrap().unwrap_err(), vec![Conflict::Changed(1)]);
        conn.execute("DELETE FROM places WHERE id = 1", []).unwrap();
        assert_eq!(table.save(&edits, &fields, std::iter::empty(), &read.revision).unwrap().unwrap_err(), vec![Conflict::Deleted(1)]);
        assert_eq!(conn.query_row("SELECT count(*) FROM places", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn stale_inserts_are_refused_but_stale_updates_are_fingerprinted() {
        let (_dir, path) = fixture();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let insert = Feature { id: 100, geometry: None, attrs: vec![Value::Text("new".into()), Value::Null, Value::Null] };
        std::thread::sleep(Duration::from_millis(5));
        Connection::open(&path).unwrap().execute("UPDATE places SET name = 'other' WHERE id = 1", []).unwrap();
        let error =
            table.save(&Edits { inserts: vec![insert], ..Default::default() }, &fields, std::iter::empty(), &read.revision).unwrap_err();
        assert!(error.to_string().contains("refusing stale inserts"), "{error}");
        assert_eq!(Connection::open(&path).unwrap().query_row("SELECT count(*) FROM places", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn image_fields_are_never_written() {
        let (_dir, path) = fixture();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["name", "lanes", "photo"]);
        assert_eq!(fields[2].ty, FieldType::Image);
        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("renamed".into());
        // The thumbnail is not the file's bytes: supplying something else must not replace them.
        changed.attrs[2] = Value::Null;
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        assert_eq!(saved.read_only, vec!["photo".to_string()]);
        assert_eq!(saved.features[0].attrs[2], read.features[0].attrs[2]);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT name, photo FROM places WHERE id = 1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .unwrap(),
            ("renamed".into(), picture())
        );
        let edits = Edits {
            inserts: vec![Feature { id: 5, geometry: None, attrs: vec![Value::Text("n".into()), Value::Null, Value::Null] }],
            ..Default::default()
        };
        let saved = table.save(&edits, &fields, read.versions.iter().map(|(k, _)| *k), &saved.revision).unwrap().unwrap();
        let photo: Option<Vec<u8>> = conn.query_row("SELECT photo FROM places WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(photo, None, "an inserted picture column stays empty");
        assert!(saved.read_only.contains(&"photo".to_string()));
    }

    #[test]
    fn zm_geometry_is_refused_but_attributes_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevation.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE lines (id INTEGER PRIMARY KEY, label TEXT, geom BLOB);").unwrap();
        let g = line(1.0);
        let mut body = vec![1u8];
        body.extend_from_slice(&1002u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        for (x, z) in [(1.0_f64, 1.0_f64), (2.0, 2.0)] {
            body.extend_from_slice(&x.to_le_bytes());
            body.extend_from_slice(&2.0f64.to_le_bytes());
            body.extend_from_slice(&z.to_le_bytes());
        }
        let blob = sqlite_blob::spatialite_blob(&body, 4326, &g.extent()).expect("a SpatiaLite blob");
        conn.execute("INSERT INTO lines VALUES (1, 'old', ?1)", [&blob]).unwrap();
        let table = SqliteEditTable::open(&path, "lines").unwrap();
        assert!(table.zm());
        assert_eq!(table.format_name(), "spatialite");
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut moved = read.features[0].clone();
        moved.geometry = Some(line(5.0));
        let error = table
            .save(&Edits { updates: vec![(moved, read.versions[0].1)], ..Default::default() }, &fields, std::iter::empty(), &read.revision)
            .unwrap_err();
        assert!(error.to_string().contains("Z or M"), "{error}");
        let mut attribute = read.features[0].clone();
        attribute.attrs[0] = Value::Text("new".into());
        let edits = Edits { updates: vec![(attribute, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &fields, std::iter::empty(), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        assert_eq!(conn.query_row("SELECT geom FROM lines WHERE id = 1", [], |r| r.get::<_, Vec<u8>>(0)).unwrap(), blob);
        let error = table
            .save(
                &Edits {
                    inserts: vec![Feature { id: 9, geometry: Some(line(7.0)), attrs: vec![Value::Text("n".into())] }],
                    ..Default::default()
                },
                &fields,
                std::iter::empty(),
                &saved.revision,
            )
            .unwrap_err();
        assert!(error.to_string().contains("needs Z or M"), "{error}");
    }

    #[test]
    fn tables_without_a_primary_key_use_the_rowid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE notes (label TEXT, n INTEGER); INSERT INTO notes VALUES ('a', 1), ('b', 2);").unwrap();
        let table = SqliteEditTable::open(&path, "notes").unwrap();
        assert_eq!(table.key(), "rowid");
        assert_eq!(table.geometry_column(), None);
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(read.features.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2]);
        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("edited".into());
        let edits = Edits {
            updates: vec![(changed, read.versions[0].1)],
            deletes: vec![(2, read.versions[1].1)],
            inserts: vec![Feature { id: 100, geometry: None, attrs: vec![Value::Text("new".into()), Value::Int(9)] }],
        };
        let saved = table.save(&edits, &fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!((saved.changes.updated, saved.changes.deleted), (1, 1));
        assert_eq!(saved.changes.inserted, vec![(100, 3)]);
        let conn = Connection::open(&path).unwrap();
        let rows = conn
            .prepare("SELECT rowid, label, n FROM notes ORDER BY rowid")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows, vec![(1, "edited".into(), 1), (3, "new".into(), 9)]);
    }

    #[test]
    fn composite_and_non_integer_keys_are_refused() {
        for schema in [
            "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b))",
            "CREATE TABLE t (a TEXT PRIMARY KEY, b TEXT)",
            "CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT) WITHOUT ROWID",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("keys.sqlite");
            Connection::open(&path).unwrap().execute_batch(schema).unwrap();
            let result = SqliteEditTable::open(&path, "t");
            match schema {
                s if s.contains("PRIMARY KEY (a, b)") => {
                    assert!(matches!(&result, Err(DataError::Format(e)) if e.contains("composite primary key")));
                }
                s if s.contains("a TEXT PRIMARY KEY") => {
                    assert!(matches!(&result, Err(DataError::Format(e)) if e.contains("isn't an INTEGER")));
                }
                _ => assert!(result.is_ok(), "a single INTEGER PRIMARY KEY is editable"),
            }
        }
    }

    #[test]
    fn plain_wkb_and_metadata_srid_are_written_in_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wkb.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE geometry_columns (f_table_name TEXT, f_geometry_column TEXT, geometry_type INTEGER, srid INTEGER);
             INSERT INTO geometry_columns VALUES ('places', 'geom', 2, 4326);
             CREATE TABLE places (id INTEGER PRIMARY KEY, name TEXT, geom BLOB);
             INSERT INTO places VALUES (1, 'old', x'00');",
        )
        .unwrap();
        let g = line(1.0);
        conn.execute("UPDATE places SET geom = ?1", [plain(&g)]).unwrap();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        assert_eq!(table.format_name(), "wkb");
        assert_eq!(table.srid(), 4326);
        let fields = table.info().fields.clone();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut moved = read.features[0].clone();
        moved.geometry = Some(line(6.0));
        let edits = Edits { updates: vec![(moved, read.versions[0].1)], ..Default::default() };
        table.save(&edits, &fields, std::iter::empty(), &read.revision).unwrap().unwrap();
        let stored: Vec<u8> = conn.query_row("SELECT geom FROM places WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sqlite_blob::srid(&stored), Some(4326), "the declared SRID is kept in EWKB");
        let (format, _, body) = sqlite_blob::split_blob(&stored).expect("stored geometry");
        assert_eq!(format, GeometryFormat::Wkb);
        assert_eq!(wkb::decode(body.as_ref()), Some(line(6.0)));
    }

    #[test]
    fn unreadable_geometries_are_skipped_and_counted() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT INTO places VALUES (2, 'bad', 1, NULL, x'00')", []).unwrap();
        let table = SqliteEditTable::open(&path, "places").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!((read.matched, read.skipped, read.features.len()), (2, 1, 1));
        assert_eq!(read.versions.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![1]);
    }
}
