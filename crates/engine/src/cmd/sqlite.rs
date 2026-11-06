//! Desktop editing of one ordinary SQLite table, saved atomically.

#[cfg(not(target_arch = "wasm32"))]
use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};
#[cfg(not(target_arch = "wasm32"))]
use rustgis_data::source::sqlite::edit::{MAX_EDITABLE, SqliteEditTable};
#[cfg(not(target_arch = "wasm32"))]
use rustgis_map::source::versions_of;
use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

pub fn commands() -> Vec<C> {
    vec![
        C::new("sqlite.addLayer", "Edit SQLite Table", "Home › Add › Add Data", add_layer).params(
            r#"{"path": "an SQLite database file on this computer", "table"?: "its table (needed when it has several)", "x"?: "a numeric field holding X, for a table with no geometry column", "y"?: "a numeric field holding Y, with x", "crs"?: "the X and Y coordinate system (EPSG:4326 when left out)", "limit"?: "the most rows (default and most: 100000)", "name"?: string}"#,
        ),
        C::new("sqlite.tableInfo", "SQLite Table Fields", "Home › Add › Add Data", table_info)
            .view()
            .params(r#"{"path": "an SQLite database file on this computer", "table"?: "its table (needed when it has several)"}"#),
        C::new("sqlite.pendingEdits", "SQLite Edits", "Edit › Manage Edits", pending_edits).view()
            .params(r#"{"layer"?: "a layer added with sqlite.addLayer (default: the active one)"}"#),
        C::new("sqlite.saveEdits", "Save to SQLite", "Edit › Manage Edits", save_edits)
            .params(r#"{"layer"?: "a layer added with sqlite.addLayer (default: the active one)"}"#),
        C::new("sqlite.discardEdits", "Discard SQLite Edits", "Edit › Manage Edits", discard_edits)
            .params(r#"{"layer"?: "a layer added with sqlite.addLayer (default: the active one)"}"#),
    ]
}

/// The database file, its name for messages, and the table it names (the only one when it has
/// just one).
#[cfg(not(target_arch = "wasm32"))]
fn path_and_table(p: &Value) -> Result<(std::path::PathBuf, String, String), String> {
    use std::path::Path;
    let path = ps(p, "path").map(str::trim).filter(|v| !v.is_empty()).ok_or("path is required")?;
    let path = Path::new(path);
    if !path.is_file() {
        return Err(format!("{}: no such file", path.display()));
    }
    let path = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let table = match ps(p, "table").map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            let tables = rustgis_data::source::sqlite::tables(&path).map_err(|e| format!("{file}: {e}"))?;
            if tables.len() != 1 {
                return Err(format!("{file}: {} tables ({}); choose one with table", tables.len(), tables.join(", ")));
            }
            tables.first().cloned().ok_or_else(|| format!("{file}: no tables"))?
        }
    };
    Ok((path, file, table))
}

/// A table's fields and stored geometry, so a dialog can offer its numeric fields as X and Y.
fn table_info(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("SQLite tables are read in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::FeatureSource;
        let _ = s;
        let (path, file, table) = path_and_table(p)?;
        let src = rustgis_data::source::sqlite::SqliteSource::open(&path, Some(&table)).map_err(|e| format!("{file}: {e}"))?;
        let columns: Vec<Value> =
            src.info().fields.iter().map(|f| json!({"name": f.name, "type": f.ty.label(), "numeric": f.ty.is_numeric()})).collect();
        Ok(json!({
            "table": src.info().name,
            "key": src.key_column().unwrap_or("rowid"),
            "geometry": src.geometry_column(),
            "columns": columns,
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sqlite_layer(s: &Session, p: &Value) -> Result<(u32, String, rustgis_map::source::SqliteEdit), String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let edit = l
        .sqlite
        .clone()
        .ok_or_else(|| format!("{}: not a SQLite layer loaded for editing (Home › Add Data › Edit SQLite Table…)", l.name))?;
    Ok((id, l.name.clone(), edit))
}

fn pending_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("SQLite tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (id, name, edit) = sqlite_layer(s, p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let c = edit.versions.changes(&l.dataset.features);
        Ok(json!({"layer": name, "file": edit.path, "table": edit.table,
            "inserts": c.inserts.len(), "updates": c.updates.len(), "deletes": c.deletes.len(), "writable": edit.writable}))
    }
}

fn add_layer(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("SQLite tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (path, file, table) = path_and_table(p)?;
        let err = |e: rustgis_data::DataError| format!("{file}: {e}");
        let mut src = SqliteEditTable::open(&path, &table).map_err(err)?;
        let x = ps(p, "x").map(str::trim).filter(|v| !v.is_empty());
        let y = ps(p, "y").map(str::trim).filter(|v| !v.is_empty());
        let crs = ps(p, "crs").map(str::trim).filter(|v| !v.is_empty());
        match (x, y) {
            (Some(x), Some(y)) => src.set_points(x, y, crs).map_err(err)?,
            _ if x.is_some() || y.is_some() => {
                return Err(format!("{file}: give both x and y, two numeric fields to make points from"));
            }
            _ => {}
        }
        let points = src.points().map(|(x, y)| (x.to_string(), y.to_string()));
        let limit = f(p, "limit").map_or(MAX_EDITABLE, |n| n.clamp(1.0, MAX_EDITABLE as f64) as usize);
        let editable = src.read_editable(limit).map_err(err)?;
        let name = ps(p, "name").map(str::trim).filter(|v| !v.is_empty()).unwrap_or(src.table()).to_string();
        let info = src.info();
        let geometry_type = if points.is_some() { rustgis_data::GeometryType::Point } else { info.geometry_type };
        let mut ds = rustgis_data::Dataset::new(name.clone(), geometry_type, info.fields.clone());
        ds.crs = src.crs();
        ds.zm_dropped = info.zm_dropped;
        let path = path.to_string_lossy().into_owned();
        ds.source = Some(path.clone());
        let n = editable.features.len();
        let read = editable.versions.iter().copied().collect();
        let versions = versions_of(&editable.features, &Default::default(), &read);
        ds.features = editable.features.into();
        for note in &info.notes {
            s.notify(format!("{name}: {note}"), false);
        }
        if (n as u64) < editable.matched {
            s.notify(
                format!("{name}: loaded {n} of {} rows; the others aren't edited, and saving leaves them as they are", editable.matched),
                false,
            );
        }
        if editable.skipped > 0 {
            let message = match &points {
                Some(_) => format!(
                    "{name}: {} rows without a usable pair of X and Y values weren't loaded; saving leaves them as they are",
                    editable.skipped
                ),
                None => format!(
                    "{name}: {} rows with a geometry RustGIS can't read weren't loaded; saving leaves them as they are",
                    editable.skipped
                ),
            };
            s.notify(message, false);
        }
        if !editable.writable {
            s.notify(format!("{name}: the file is read-only, so edits can't be saved to it"), false);
        }
        if let Some((x, y)) = &points {
            let crs = src.point_crs().map_or_else(|| "EPSG:4326".into(), |c| c.label());
            s.notify(format!("{name}: points are made from {x} and {y}, read as coordinates in {crs}"), false);
        } else if src.geometry_column().is_none() {
            s.notify(format!("{name}: it has no geometry column; its attributes are read as a table"), false);
        } else if src.zm() {
            s.notify(format!("{name}: its geometry carries Z or M values, which edits can't write; attributes still save"), false);
        }
        let empty_before = s.map().layers.is_empty();
        let id = super::layer::add_dataset(s, ds);
        let (x, y) = match &points {
            Some((x, y)) => (Some(x.clone()), Some(y.clone())),
            None => (None, None),
        };
        if let Some(l) = s.map_mut().layer_mut(id) {
            l.sqlite = Some(rustgis_map::source::SqliteEdit {
                path,
                table: src.table().to_string(),
                key: src.key().to_string(),
                geometry: src.geometry_column().map(str::to_string),
                x,
                y,
                crs: crs.map(str::to_string),
                format: src.format_name().to_string(),
                shape: src.shape_name().to_string(),
                srid: src.srid(),
                zm: src.zm(),
                revision: editable.revision,
                writable: editable.writable,
                limit,
                versions,
            });
        }
        if empty_before && let Some(e) = s.map().layer(id).map(|l| l.extent()).filter(|e| !e.is_empty()) {
            s.zoom_to(&e);
        }
        Ok(json!({"layer": id, "features": n, "matched": editable.matched, "skipped": editable.skipped,
            "writable": editable.writable, "geometry": src.format_name()}))
    }
}

fn save_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("SQLite tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::path::Path;
        let (id, name, edit) = sqlite_layer(s, p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let changes = edit.versions.changes(&l.dataset.features);
        let fields = l.dataset.fields.clone();
        if changes.is_empty() {
            s.notify(format!("{name}: no edits to save"), false);
            return Ok(json!({"saved": false, "inserted": 0, "updated": 0, "deleted": 0}));
        }
        let table = &edit.table;
        if s.saved_over.iter().any(|v| v.shares(&edit.versions)) {
            return Err(format!(
                "{name}: the layer is back before an edit saved to {table} (undone), so nothing was saved. Discard Edits loads the rows as they are now"
            ));
        }
        let path = Path::new(&edit.path);
        if !path.is_file() {
            return Err(format!("{name}: {} isn't there any more, so nothing was saved", edit.path));
        }
        if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
            return Err(format!("{name}: {} is read-only, so nothing was saved", edit.path));
        }
        let err = |e: rustgis_data::DataError| format!("{name}: nothing was saved to {table}: {e}");
        let mut src = SqliteEditTable::open(path, table).map_err(err)?;
        if let (Some(x), Some(y)) = (&edit.x, &edit.y) {
            src.set_points(x, y, edit.crs.as_deref()).map_err(err)?;
        }
        // The table is opened just now, so its shape is compared with the one loaded.
        let geometry_same = match (src.geometry_column(), edit.geometry.as_deref()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => true,
            _ => false,
        };
        if !src.key().eq_ignore_ascii_case(&edit.key)
            || !geometry_same
            || src.format_name() != edit.format
            || src.shape_name() != edit.shape
            || src.srid() != edit.srid
            || src.zm() != edit.zm
        {
            return Err(format!("{name}: {table} changed shape since it was loaded, so nothing was saved. Discard Edits loads it again"));
        }
        let rows = changes.insert_rows;
        let edits = rustgis_data::source::edits::Edits { updates: changes.updates, deletes: changes.deletes, inserts: changes.inserts };
        let saved_rows = match src.save(&edits, &fields, edit.versions.iter().map(|(key, _)| key), &edit.revision).map_err(err)? {
            Ok(saved) => saved,
            Err(conflicts) => {
                let mut said: Vec<String> = conflicts.iter().take(20).map(ToString::to_string).collect();
                if conflicts.len() > 20 {
                    said.push(format!("{} more", conflicts.len() - 20));
                }
                return Err(format!(
                    "{name}: nothing was saved to {table} in {}, {} conflicts: {}. Discard Edits loads the rows as they are now",
                    edit.path,
                    conflicts.len(),
                    said.join("; ")
                ));
            }
        };
        let saved = saved_rows.changes;
        let mut stored: std::collections::HashMap<_, _> = saved_rows.features.into_iter().map(|f| (f.id, f)).collect();
        let versions = saved.versions.iter().copied().collect();
        if let Some(l) = s.map_mut().layer_mut(id) {
            for (row, (_, key)) in rows.iter().zip(&saved.inserted) {
                if let Some(f) = l.dataset.features.get_mut(*row) {
                    f.id = *key;
                }
            }
            let mut geometry_changed = false;
            if !saved_rows.removed.is_empty() {
                let selected: std::collections::HashSet<_> =
                    l.selection.iter().filter_map(|&row| l.dataset.features.get(row).map(|f| f.id)).collect();
                let before = l.dataset.features.len();
                l.dataset.features.retain(|f| saved_rows.removed.binary_search(&f.id).is_err());
                if l.dataset.features.len() != before {
                    geometry_changed = true;
                    l.selection =
                        l.dataset.features.iter().enumerate().filter_map(|(row, f)| selected.contains(&f.id).then_some(row)).collect();
                }
            }
            for feature in l.dataset.features.iter_mut() {
                if let Some(mut row) = stored.remove(&feature.id) {
                    for &i in &saved_rows.unstored_fields {
                        if let (Some(stored), Some(local)) = (row.attrs.get_mut(i), feature.attrs.get_mut(i)) {
                            std::mem::swap(stored, local);
                        }
                    }
                    geometry_changed |= row.geometry != feature.geometry;
                    *feature = row;
                }
            }
            for (key, _) in &edits.deletes {
                if let Some(row) = stored.remove(key) {
                    l.dataset.features.push(row);
                    geometry_changed = true;
                }
            }
            if geometry_changed {
                l.touch_geometry();
            }
            let baseline = versions_of(&l.dataset.features, &edit.versions, &versions);
            if let Some(e) = &mut l.sqlite {
                e.versions = baseline;
                e.revision = saved_rows.revision;
            }
        }
        s.saved_over.retain(rustgis_map::source::Versions::held_elsewhere);
        s.saved_over.push(edit.versions);
        let canonical = std::fs::canonicalize(path).ok();
        for map in &mut s.project.maps {
            for l in map.layers.iter_mut() {
                if let Some(source) = &mut l.source
                    && source.table.as_deref().is_none_or(|t| t.eq_ignore_ascii_case(table))
                    && source.path.as_deref().is_some_and(|p| match (canonical.as_ref(), std::fs::canonicalize(p).ok()) {
                        (Some(a), Some(b)) => *a == b,
                        _ => p == edit.path,
                    })
                {
                    source.reset();
                }
            }
        }
        s.notify(
            format!(
                "{name}: saved to {table} in {}: {} new, {} changed, {} deleted",
                edit.path,
                saved.inserted.len(),
                saved.updated,
                saved.deleted
            ),
            false,
        );
        if !saved.skipped_fields.is_empty() {
            s.notify(format!("{name}: {table} has no column for {}, so it wasn't saved", saved.skipped_fields.join(", ")), false);
        }
        if !saved_rows.read_only.is_empty() {
            s.notify(format!("{name}: {} stay in the file, as image fields are read only", saved_rows.read_only.join(", ")), false);
        }
        if saved_rows.triggers {
            s.notify(format!("{name}: triggers in {table} changed rows beyond the edits; they were read again"), false);
        }
        Ok(json!({"saved": true, "inserted": saved.inserted.len(), "updated": saved.updated, "deleted": saved.deleted,
            "unchanged": saved.unchanged, "keys": saved.inserted.iter().map(|(_, k)| k.cast_signed()).collect::<Vec<_>>(),
            "skippedFields": saved.skipped_fields, "readOnly": saved_rows.read_only, "triggers": saved_rows.triggers}))
    }
}

fn discard_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("SQLite tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (id, name, edit) = sqlite_layer(s, p)?;
        let err = |e: rustgis_data::DataError| format!("{name}: {e}");
        let mut src = SqliteEditTable::open(std::path::Path::new(&edit.path), &edit.table).map_err(err)?;
        if let (Some(x), Some(y)) = (&edit.x, &edit.y) {
            src.set_points(x, y, edit.crs.as_deref()).map_err(err)?;
        }
        let editable = src.read_editable(if edit.limit == 0 { MAX_EDITABLE } else { edit.limit }).map_err(err)?;
        let read = editable.versions.iter().copied().collect();
        let n = editable.features.len();
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        let geometry_type = if src.points().is_some() { rustgis_data::GeometryType::Point } else { src.info().geometry_type };
        if l.dataset.geometry_type != geometry_type {
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(geometry_type, id));
        }
        l.dataset.geometry_type = geometry_type;
        l.dataset.crs = src.crs();
        l.dataset.fields = src.info().fields.clone();
        l.dataset.features = editable.features.into();
        l.selection.clear();
        l.touch_geometry();
        let baseline = versions_of(&l.dataset.features, &Default::default(), &read);
        if let Some(e) = &mut l.sqlite {
            e.versions = baseline;
            e.writable = editable.writable;
            e.key = src.key().to_string();
            e.geometry = src.geometry_column().map(str::to_string);
            e.format = src.format_name().to_string();
            e.shape = src.shape_name().to_string();
            e.srid = src.srid();
            e.zm = src.zm();
            e.revision = editable.revision;
        }
        s.notify(format!("{name}: edits discarded; {n} rows loaded again from {}", edit.table), false);
        Ok(json!({"features": n, "matched": editable.matched, "skipped": editable.skipped}))
    }
}
