//! Photos attached to features: the device's camera takes one (`Services::take_photo`), and the
//! feature names it in its layer's photos field, a text field listing each photo's path,
//! separated by `;` (as a report's attachments field does). A layer without one gets a `photos`
//! field when its first photo is attached, and the sheet says so. The field stays in the form, to
//! be typed into as any other: the photos taken go after what is typed there.

use rustgis_ui_egui::rustgis_engine::data::FieldType;
use rustgis_ui_egui::rustgis_engine::map::Layer;
use rustgis_ui_egui::{RustGisApp, i18n};
use serde_json::json;
use std::path::PathBuf;

/// The name of the field a layer is given for its photos.
pub(crate) const FIELD: &str = "photos";

/// The layer's photos field: its text field named `photos` or `photo`, whatever the case.
pub(crate) fn field(layer: &Layer) -> Option<String> {
    layer
        .dataset
        .fields
        .iter()
        .find(|f| f.ty == FieldType::Text && (f.name.eq_ignore_ascii_case(FIELD) || f.name.eq_ignore_ascii_case("photo")))
        .map(|f| f.name.clone())
}

/// Layer `id`'s photos field, added (and said) when it has none, and whether it was added just
/// now. `None` when it couldn't be added (the front end says why), and when the layer has a field
/// of that name that isn't text, which is said by name.
pub(crate) fn ensure_field(app: &mut RustGisApp, id: u32) -> Option<(String, bool)> {
    let layer = app.session.map().layer(id)?;
    if let Some(found) = field(layer) {
        return Some((found, false));
    }
    let name = layer.name.clone();
    let other = layer.dataset.fields.iter().find(|f| f.name.eq_ignore_ascii_case(FIELD) || f.name.eq_ignore_ascii_case("photo"));
    if let Some(other) = other {
        let text = i18n::fmt(
            "Nothing was added or attached: {layer}'s field {field} isn't a text field to list the photos in.",
            &[("layer", &name), ("field", &other.name)],
        );
        app.session.notify(text, true);
        return None;
    }
    app.run_by_user("edit.addField", json!({"layer": id, "name": FIELD, "type": "text"})).ok()?;
    app.session.notify(i18n::fmt("Added a photos field to {layer}: it lists each feature's photos.", &[("layer", &name)]), false);
    app.session.map().layer(id).and_then(field).map(|f| (f, true))
}

/// A photos field's text with `more` photos after what it held: what it held as it was (its outer
/// spaces aside), then each photo it doesn't list already, separated by `;`; and how many were
/// added.
pub(crate) fn joined(held: &str, more: &[PathBuf]) -> (String, usize) {
    let mut text = held.trim().to_string();
    let listed: Vec<String> = text.split(';').map(|p| p.trim().to_string()).collect();
    let mut added: Vec<String> = Vec::new();
    for path in more {
        let path = path.display().to_string();
        if !listed.contains(&path) && !added.contains(&path) {
            added.push(path);
        }
    }
    let count = added.len();
    for path in added {
        if !text.is_empty() && !text.ends_with(';') {
            text.push(';');
        }
        text.push_str(&path);
    }
    (text, count)
}

/// Attaches `more` photos to the feature in row `row` of layer `id`, after the ones it has: one
/// edit, which undoes (the first photo of a layer without a photos field also adds the field, an
/// edit of its own). Says whether they were attached.
pub(crate) fn attach(app: &mut RustGisApp, id: u32, row: usize, more: &[PathBuf]) -> bool {
    if more.is_empty() {
        return true;
    }
    if !super::edit::ensure_editing(app) {
        return false;
    }
    let Some((name, _)) = ensure_field(app, id) else { return false };
    let held = app
        .session
        .map()
        .layer(id)
        .and_then(|l| Some(l.dataset.value(row, l.dataset.field_index(&name)?).to_string()))
        .filter(|v| v != "<Null>")
        .unwrap_or_default();
    // Only the photos it doesn't list already: none new is no edit.
    let (value, count) = joined(&held, more);
    if count == 0 {
        app.session.notify(i18n::fmt("The feature's {field} field lists the photo(s) already.", &[("field", &name)]), false);
        return true;
    }
    if app.run_by_user("edit.setValue", json!({"layer": id, "row": row, "field": name, "value": value})).is_err() {
        return false;
    }
    let n = count.to_string();
    app.session.notify(i18n::fmt("Attached {n} photo(s) to the feature, in its {field} field.", &[("n", &n), ("field", &name)]), false);
    true
}

/// A photo's file name, for its chip.
pub(crate) fn name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photos_are_listed_by_path_once_each() {
        let a = PathBuf::from("/data/photos/a.jpg");
        let b = PathBuf::from("/data/photos/b.jpg");
        let path = |s: &str| s.to_string();
        assert_eq!(joined("", std::slice::from_ref(&a)), (path("/data/photos/a.jpg"), 1));
        // One already listed isn't counted.
        assert_eq!(joined("/data/photos/a.jpg", &[a.clone(), b.clone()]), (path("/data/photos/a.jpg;/data/photos/b.jpg"), 1));
        assert_eq!(joined("/data/photos/a.jpg", std::slice::from_ref(&a)), (path("/data/photos/a.jpg"), 0));
        // What was held stays as it was typed, spaces and repeats and all.
        assert_eq!(joined("Front; back", std::slice::from_ref(&b)), (path("Front; back;/data/photos/b.jpg"), 1));
        assert_eq!(joined("a.jpg; a.jpg", &[]), (path("a.jpg; a.jpg"), 0));
        assert_eq!(joined("  x  ", &[]), (path("x"), 0));
        assert_eq!(joined("", &[a.clone(), a.clone()]), (path("/data/photos/a.jpg"), 1));
        // A held list ending in `;` gets no empty entry.
        assert_eq!(joined("a.jpg;", std::slice::from_ref(&b)), (path("a.jpg;/data/photos/b.jpg"), 1));
        assert_eq!(name(&b), "b.jpg");
    }
}
