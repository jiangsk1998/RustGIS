//! The attribute table, docked under the map: sort, filter to the selection, select rows (synced
//! with the map) and edit values in place.

use egui::{Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use rustgis_engine::data::{Dataset, Value as V};
use serde_json::{Value, json};

use crate::theme::{Tokens, semibold};
use crate::widgets::small;
use crate::{RustGisApp, icons};

#[derive(Default)]
pub struct TableState {
    pub folder: Option<std::path::PathBuf>,
    sort: Option<(usize, bool)>,
    selected_only: bool,
    editing: Option<CellEdit>,
    /// The pass the cell editor last had the focus in (see [`had_focus`]).
    focus_pass: Option<u64>,
    filter: String,
}

/// Whether a cell was being typed in during the pass before `pass`: egui takes the focus away on
/// Escape before the shortcuts run, and that Escape cancels the cell, not the tool.
pub fn had_focus(st: &TableState, pass: u64) -> bool {
    st.focus_pass.is_some_and(|p| p + 1 == pass)
}

/// A value being typed into a cell.
struct CellEdit {
    /// The layer the table showed: shown another (Ctrl+T), the edit is dropped.
    layer: Option<u32>,
    /// The feature's id, not its row: rows deleted meanwhile, it still finds its feature (or,
    /// gone, writes nothing).
    fid: u64,
    col: usize,
    text: String,
    /// The text it opened with: left as it was, nothing is written (a null stays null).
    was: String,
    /// Just opened: the editor takes the keyboard focus this frame. Taken every frame, Enter
    /// couldn't let it go and nothing was ever kept (#1182).
    focus: bool,
}

impl CellEdit {
    fn new(layer: Option<u32>, fid: u64, col: usize, text: String) -> CellEdit {
        CellEdit { layer, fid, col, was: text.clone(), text, focus: true }
    }
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let layer_id = app.session.table_layer;
    // An edit is the layer's it was opened on: the table closed (however) or showing another
    // layer, it is dropped rather than written later to whatever row has its place.
    if app.table.editing.as_ref().is_some_and(|e| e.layer != layer_id) {
        app.table.editing = None;
    }
    if layer_id.is_none() && app.session.table_standalone.is_none() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let pass = ui.ctx().cumulative_pass_nr();
    egui::Panel::bottom("attribute_table")
        .resizable(true)
        .default_size(250.0)
        .size_range(120.0..=600.0)
        .frame(egui::Frame::NONE.fill(t.pane))
        .show(ui, |ui| {
            // Data source: a layer or a standalone table.
            let (ds, selection, title): (Dataset, Vec<usize>, String) = match layer_id.and_then(|id| app.session.map().layer(id)) {
                Some(l) => (l.dataset.clone(), l.selection.clone(), l.name.clone()),
                None => match app.session.table_standalone.and_then(|id| app.session.project.table(id)) {
                    Some(tb) => (tb.dataset.clone(), Vec::new(), tb.name.clone()),
                    None => {
                        app.session.table_standalone = None;
                        return;
                    }
                },
            };
            let is_layer = layer_id.is_some();
            // Header.
            let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            ui.painter().rect_filled(hr, 0.0, t.pane_header);
            ui.painter().line_segment([egui::pos2(hr.min.x, hr.min.y), egui::pos2(hr.max.x, hr.min.y)], Stroke::new(1.0, t.border_strong));
            let mut header_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(hr.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            {
                let ui = &mut header_ui;
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, "table", t.icon, t.accent);
                ui.label(egui::RichText::new(&title).font(semibold(13.0)));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{n} of {total} selected",
                        &[("n", &selection.len().to_string()), ("total", &ds.features.len().to_string())],
                    ))
                    .size(11.5)
                    .color(t.text_dim),
                );
                ui.add_space(10.0);
                if is_layer {
                    small(
                        ui,
                        app,
                        "select_attr",
                        "Select By Attributes",
                        "Select rows with a SQL expression.",
                        "ui.dialog",
                        json!({"name": "selectByAttributes"}),
                        false,
                    );
                    small(ui, app, "zoom_sel", "", "Zoom to selection", "select.zoomTo", Value::Null, false);
                    small(ui, app, "select", "", "Switch selection", "select.switch", Value::Null, false);
                    small(ui, app, "clear_sel", "", "Clear selection", "select.clear", Value::Null, false);
                    small(ui, app, "delete", "", "Delete selected rows", "edit.delete", Value::Null, false);
                    small(ui, app, "field", "", "Add field", "ui.dialog", json!({"name": "addField"}), false);
                    small(ui, app, "calculate", "", "Calculate field", "ui.dialog", json!({"name": "calculateField"}), false);
                    // The table's layer, even when another layer is active in Layers.
                    small(
                        ui,
                        app,
                        "calc_geom",
                        "",
                        "Calculate Geometry",
                        "ui.dialog",
                        json!({"name": "calculateGeometry", "layer": layer_id}),
                        false,
                    );
                    ui.add_space(6.0);
                    ui.checkbox(&mut app.table.selected_only, egui::RichText::new(tl!("Selected only")).size(11.5));
                }
                ui.add(egui::TextEdit::singleline(&mut app.table.filter).hint_text(tl!("Filter rows…")).desired_width(160.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(tl!("×")).frame(false)).on_hover_text(tl!("Close table")).clicked() {
                        app.session.table_layer = None;
                        app.session.table_standalone = None;
                        app.table.editing = None;
                    }
                });
            }
            // Rows to show.
            let mut rows: Vec<usize> =
                if app.table.selected_only && is_layer { selection.clone() } else { (0..ds.features.len()).collect() };
            let f = app.table.filter.trim().to_lowercase();
            if !f.is_empty() {
                rows.retain(|r| ds.features.get(*r).is_some_and(|ft| ft.attrs.iter().any(|v| v.to_string().to_lowercase().contains(&f))));
            }
            if let Some((c, asc)) = app.table.sort {
                rows.sort_by(|a, b| {
                    let o = ds.value(*a, c).partial_cmp(ds.value(*b, c)).unwrap_or(std::cmp::Ordering::Equal);
                    if asc { o } else { o.reverse() }
                });
            }
            let nf = ds.fields.len();
            let mut clicked: Option<(usize, bool)> = None;
            let mut commits: Vec<(u64, usize, String, String)> = Vec::new();
            // Whether the cell being typed in was laid out (its row scrolled into view).
            let mut drawn = false;
            let mut cancel: Option<(u64, usize)> = None;
            let mut sort_click: Option<usize> = None;
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mut tb = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(90.0).at_least(54.0));
                for _ in 0..nf {
                    tb = tb.column(Column::initial(120.0).at_least(50.0).clip(true));
                }
                tb.min_scrolled_height(0.0)
                    .header(24.0, |mut h| {
                        h.col(|ui| {
                            ui.label(egui::RichText::new(tl!("Feature ID")).font(semibold(11.5)).color(t.text_dim))
                                .on_hover_text(tl!("Feature identifier, not a dataset field or a row position."));
                        });
                        for (i, fd) in ds.fields.iter().enumerate() {
                            h.col(|ui| {
                                let arrow = match app.table.sort {
                                    Some((c, true)) if c == i => " ↑",
                                    Some((c, false)) if c == i => " ↓",
                                    _ => "",
                                };
                                let r = ui.add(
                                    egui::Label::new(egui::RichText::new(format!("{}{arrow}", fd.display())).font(semibold(11.5)))
                                        .sense(Sense::click()),
                                );
                                if r.on_hover_text(crate::i18n::fmt(
                                    "{field} ({type}) · click to sort",
                                    &[("field", &fd.name), ("type", tl_raw!(fd.ty.label()))],
                                ))
                                .clicked()
                                {
                                    sort_click = Some(i);
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, rows.len(), |mut row| {
                            let Some(&r) = rows.get(row.index()) else { return };
                            let sel = selection.contains(&r);
                            row.set_selected(sel);
                            let oid = ds.features.get(r).map(|f| f.id).unwrap_or(0);
                            row.col(|ui| {
                                ui.label(egui::RichText::new(oid.to_string()).size(11.5).color(t.text_dim));
                            });
                            for c in 0..nf {
                                row.col(|ui| {
                                    let editing = app.table.editing.as_ref().is_some_and(|e| e.fid == oid && e.col == c);
                                    if editing {
                                        if let Some(e) = app.table.editing.as_mut() {
                                            drawn = true;
                                            // Named for its field (a screen reader says what it edits; an empty cell shows it).
                                            let name = ds.fields.get(c).map(|f| f.name.as_str()).unwrap_or_default();
                                            let te = ui
                                                .add(egui::TextEdit::singleline(&mut e.text).hint_text(name).desired_width(f32::INFINITY));
                                            if std::mem::take(&mut e.focus) {
                                                te.request_focus();
                                            }
                                            if te.has_focus() {
                                                app.table.focus_pass = Some(pass);
                                            }
                                            // Enter, Tab or a click elsewhere keeps the value; Escape
                                            // leaves the cell as it was (the shortcuts leave it to the
                                            // cell, see `had_focus`).
                                            if te.lost_focus() {
                                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                    cancel = Some((oid, c));
                                                } else {
                                                    commits.push((oid, c, e.text.clone(), e.was.clone()));
                                                }
                                            }
                                        }
                                    } else {
                                        let v = ds.value(r, c);
                                        // A picture (a photo's thumbnail) is named, not spelled out.
                                        let picture = matches!(v, V::Text(s) if crate::mapview::is_picture(s));
                                        let shown = if picture { tl!("picture").to_string() } else { v.to_string() };
                                        let txt = egui::RichText::new(shown).size(11.5);
                                        let txt = if matches!(v, V::Null) || picture { txt.color(t.text_disabled).italics() } else { txt };
                                        let resp = ui.add(egui::Label::new(txt).sense(Sense::click()).truncate());
                                        if resp.double_clicked() && is_layer {
                                            // The cell being typed in is kept first (once: its
                                            // editor may have kept it on losing the focus).
                                            if let Some(e) = app.table.editing.take()
                                                && !commits.iter().any(|q| q.0 == e.fid && q.1 == e.col)
                                                && cancel != Some((e.fid, e.col))
                                            {
                                                commits.push((e.fid, e.col, e.text, e.was));
                                            }
                                            let text = if matches!(v, V::Null) { String::new() } else { v.to_string() };
                                            app.table.editing = Some(CellEdit::new(layer_id, oid, c, text));
                                        } else if resp.clicked() {
                                            clicked = Some((r, ui.input(|i| i.modifiers.command || i.modifiers.shift)));
                                        }
                                    }
                                });
                            }
                            if row.response().clicked() {
                                clicked = Some((r, row.response().ctx.input(|i| i.modifiers.command || i.modifiers.shift)));
                            }
                        });
                    });
            });
            if let Some(i) = sort_click {
                app.table.sort = match app.table.sort {
                    Some((c, true)) if c == i => Some((i, false)),
                    Some((c, false)) if c == i => None,
                    _ => Some((i, true)),
                };
            }
            if let Some((r, add)) = clicked
                && is_layer
            {
                let mode = if add { if selection.contains(&r) { "remove" } else { "add" } } else { "new" };
                let _ = app.run("select.rows", json!({"layer": layer_id, "rows": [r], "mode": mode}));
            }
            // A cell scrolled out of view loses the focus unseen: kept, as a click elsewhere (left
            // as it was on Escape).
            if !drawn
                && let Some(e) = app.table.editing.take_if(|e| !e.focus)
                && !(had_focus(&app.table, pass) && ui.input(|i| i.key_pressed(egui::Key::Escape)))
            {
                commits.push((e.fid, e.col, e.text, e.was));
            }
            let ends = |editing: &Option<CellEdit>, fid: u64, c: usize| editing.as_ref().is_some_and(|e| e.fid == fid && e.col == c);
            if let Some((fid, c)) = cancel
                && ends(&app.table.editing, fid, c)
            {
                app.table.editing = None;
            }
            for (fid, c, text, was) in commits {
                if ends(&app.table.editing, fid, c) {
                    app.table.editing = None;
                }
                // Its feature where it is now, in the layer as it is (the table's own Delete runs
                // after `ds` was read); gone, nothing is written.
                let live =
                    layer_id.and_then(|id| app.session.map().layer(id)).and_then(|l| l.dataset.features.iter().position(|f| f.id == fid));
                let (Some(fd), Some(r)) = (ds.fields.get(c), live) else { continue };
                if text == was {
                    continue;
                }
                // Text that isn't a value of the field (a number mistyped) is refused, not
                // written as null or false.
                if !fits(&text, fd.ty) {
                    let msg = crate::i18n::fmt(
                        "“{text}” isn't a {type} value: {field} is left as it was.",
                        &[("text", text.trim()), ("type", tl_raw!(fd.ty.label())), ("field", &fd.name)],
                    );
                    app.session.notify(msg, true);
                    continue;
                }
                // Refused (editing turned off, a domain or rule): said, unless the command said so.
                let mark = app.session.notice_mark();
                if let Err(e) = app.run("edit.setValue", json!({"layer": layer_id, "row": r, "field": fd.name, "value": text}))
                    && app.session.notices_since(mark).is_empty()
                {
                    app.session.notify(e, true);
                }
            }
        });
}

/// Whether `text` reads as a value of a `ty` field (`Value::parse`'s reading): blank is null;
/// a number that doesn't parse (or isn't finite, or for a whole number is out of range), or a
/// word that isn't yes or no, isn't.
fn fits(text: &str, ty: rustgis_engine::data::FieldType) -> bool {
    use rustgis_engine::data::FieldType as T;
    let t = text.trim();
    t.is_empty()
        || t.eq_ignore_ascii_case("<null>")
        || match ty {
            T::Text => true,
            // A whole number, also written as a float (`1e3`), in range (`Value::parse` would
            // truncate a fraction or saturate past the range).
            T::Integer => {
                t.parse::<i64>().is_ok() || t.parse::<f64>().is_ok_and(|f| f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64)
            }
            T::Double => t.parse::<f64>().is_ok_and(f64::is_finite),
            T::Boolean => matches!(t.to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "1" | "0" | "y" | "n"),
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value typed into a cell is kept on Enter, and Escape leaves the cell as it was (not the
    /// tool: the shortcuts run first, egui has dropped the focus on Escape by then). The editor
    /// took the focus back every frame, so Enter never ended it and nothing was kept (#1182).
    #[test]
    fn a_value_typed_into_a_cell_is_kept_on_enter() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        app.session.table_layer = Some(id);
        app.run("tool.set", json!({"tool": "select"})).unwrap();
        let ds = &app.session.map().layer(id).unwrap().dataset;
        let col = (0..ds.fields.len()).find(|&c| matches!(ds.value(0, c), V::Text(_))).expect("a text field");
        let num = (0..ds.fields.len()).find(|&c| matches!(ds.value(0, c), V::Int(_) | V::Float(_))).expect("a number field");
        let fid: Vec<u64> = ds.features.iter().map(|f| f.id).collect();
        let value = |app: &RustGisApp, row: usize| app.session.map().layer(id).unwrap().dataset.value(row, col).to_string();
        let (first, second) = (value(&app, 0), value(&app, 1));
        let number = app.session.map().layer(id).unwrap().dataset.value(0, num).to_string();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        let tool = h.state().session.tool;
        // Opened as a double-click opens it, the text selected by nothing: typing adds to it.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, first.clone()));
        h.run_steps(2);
        h.event(egui::Event::Text("Zed".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().table.editing.is_none(), "the editor closed");
        // Escape: typed, then left as it was, the tool kept.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[1], col, second.clone()));
        h.run_steps(2);
        h.event(egui::Event::Text("Nope".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(value(h.state(), 1), second);
        assert!(h.state().table.editing.is_none(), "the editor closed");
        assert_eq!(h.state().session.tool, tool, "Escape left the cell, not the tool");
        // Shown another layer meanwhile (Ctrl+T), an open edit is dropped, not written there;
        // so is one left open when the table closes.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id + 1000), fid[0], col, String::new()));
        h.run_steps(2);
        assert!(h.state().table.editing.is_none());
        // A number mistyped is refused, not written as null.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], num, String::new()));
        h.run_steps(2);
        h.event(egui::Event::Text("12a".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.value(0, num).to_string(), number);
        assert!(h.state().table.editing.is_none());
        // With editing turned off, a value isn't kept, and the table says why.
        h.state_mut().session.map_mut().editing.settings.enabled = false;
        let mark = h.state().session.notice_mark();
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, String::new()));
        h.run_steps(2);
        h.event(egui::Event::Text("Off".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().session.notices_since(mark).iter().any(|n| n.text.contains("editing is turned off")), "said why");
        h.state_mut().session.map_mut().editing.settings.enabled = true;
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, String::new()));
        h.state_mut().session.table_layer = None;
        h.run_steps(2);
        assert!(h.state().table.editing.is_none(), "dropped when the table closed");
    }

    #[test]
    fn typed_text_fits_its_field() {
        use rustgis_engine::data::FieldType as T;
        for (text, ty, ok) in [
            ("12", T::Integer, true),
            ("1.5", T::Double, true),
            ("12a", T::Integer, false),
            ("1e3", T::Integer, true),
            ("-7", T::Integer, true),
            ("1e30", T::Integer, false),
            ("1.5", T::Integer, false),
            ("9.21e18", T::Integer, true),
            ("1e19", T::Integer, false),
            ("NaN", T::Integer, false),
            ("inf", T::Double, false),
            ("tru", T::Boolean, false),
            ("No", T::Boolean, true),
            ("", T::Double, true),
            (" <null> ", T::Integer, true),
            ("anything", T::Text, true),
        ] {
            assert_eq!(fits(text, ty), ok, "{text:?} as {ty:?}");
        }
    }
}
