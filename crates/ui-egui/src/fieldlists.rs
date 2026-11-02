//! One ordered-list editor for field names and field/statistic objects in tool parameters.
use std::collections::HashMap;

use egui::Ui;
use rustgis_engine::{
    analysis::{ParamKind, StatisticList},
    data::{Field, FieldType},
};
use serde_json::{Value, json};

/// Named input supplying a list's fields. Other parameter kinds use their existing editors.
pub(crate) fn input(kind: ParamKind) -> Option<&'static str> {
    match kind {
        ParamKind::Fields { input } | ParamKind::FieldStatistics { input, .. } => Some(input),
        _ => None,
    }
}

/// Reads a Tools-pane list value, stored as JSON so legacy strings retain their type as well
/// as their contents. An empty form value is absent, as for other optional parameters.
pub(crate) fn form_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!(text))
}

/// Recognizes values the structured editor can represent, including legacy scalar fields and
/// JSON-text statistic lists. Invalid shapes stay in the repair editor until a complete repair.
fn editable_rows(value: &Value, statistics: Option<StatisticList>) -> Option<Vec<Value>> {
    let decoded = if statistics.is_some() {
        match value {
            Value::String(s) if s.trim().is_empty() => Value::Null,
            Value::String(s) => match serde_json::from_str(s) {
                Ok(array @ Value::Array(_)) => array,
                _ => value.clone(),
            },
            _ => value.clone(),
        }
    } else {
        value.clone()
    };
    match &decoded {
        Value::Null => Some(Vec::new()),
        Value::String(s) if statistics.is_none() => {
            if s.trim().is_empty() {
                Some(Vec::new())
            } else {
                Some(vec![json!(s)])
            }
        }
        Value::Array(rows)
            if rows.len() <= 256
                && rows.iter().all(|r| match statistics {
                    None => r.is_string(),
                    Some(s) => {
                        r.is_object()
                            && r.get("field").is_some_and(Value::is_string)
                            && r.get(s.key()).is_some_and(Value::is_string)
                            && r.get("delimiter").is_none_or(|v| v.is_null() || v.is_string())
                    }
                }) =>
        {
            Some(rows.clone())
        }
        _ => None,
    }
}

/// One frame's value change and undo boundary. Typing shares the checkpoint taken on focus;
/// discrete list actions take their own checkpoint before applying the returned value.
#[derive(Default)]
pub(crate) struct Edit {
    pub value: Option<Value>,
    pub checkpoint: bool,
}

/// Edits a list without changing its JSON until a user acts. Unknown fields and statistics are
/// shown as saved; malformed or oversized values remain editable as JSON rather than discarded.
/// `None` fields means an upstream model input is not known yet, so names can be typed.
pub(crate) fn show(ui: &mut Ui, kind: ParamKind, value: &Value, fields: Option<&[Field]>) -> Edit {
    let statistics = match kind {
        ParamKind::FieldStatistics { statistics, .. } => Some(statistics),
        _ => None,
    };
    let mut rows = match editable_rows(value, statistics) {
        Some(rows) => rows,
        _ => {
            ui.label(tl!("This value cannot be shown as a list. Edit its JSON below."));
            // Keep incomplete JSON as an editor draft. In particular a field-list string is a
            // valid legacy value, so storing an unfinished JSON fragment as a string would
            // replace this editor with a field picker before the user could finish typing.
            let draft = ui.scope_id().with(("json_draft", value.to_string()));
            let mut text = ui
                .ctx()
                .data_mut(|d| d.get_temp::<String>(draft))
                .unwrap_or_else(|| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()));
            let label = ui.label(tl!("JSON value"));
            let response = ui.add(egui::TextEdit::multiline(&mut text).desired_width(f32::INFINITY)).labelled_by(label.id);
            let mut edit = Edit { checkpoint: response.gained_focus(), ..Default::default() };
            if response.changed() {
                if let Ok(value) = serde_json::from_str(&text)
                    && editable_rows(&value, statistics).is_some()
                {
                    ui.ctx().data_mut(|d| d.remove::<String>(draft));
                    edit.value = Some(value);
                    return edit;
                }
                ui.ctx().data_mut(|d| d.insert_temp(draft, text));
            }
            return edit;
        }
    };
    let mut checkpoint = false;
    let mut changed = false;
    let mut action = None;
    // Execution resolves an exact spelling first, then an unambiguous case-insensitive name.
    let mut exact: HashMap<&str, &Field> = HashMap::new();
    let mut folded: HashMap<String, Option<&Field>> = HashMap::new();
    for f in fields.into_iter().flatten() {
        exact.entry(f.name.as_str()).or_insert(f);
        folded.entry(f.name.to_ascii_lowercase()).and_modify(|f| *f = None).or_insert(Some(f));
    }
    let resolve = |name: &str| exact.get(name.trim()).copied().or_else(|| folded.get(&name.trim().to_ascii_lowercase()).copied().flatten());
    let identity = |name: &str| {
        if statistics.is_some() {
            resolve(name).map(|f| f.name.clone()).unwrap_or_else(|| name.trim().to_ascii_lowercase())
        } else {
            name.trim().to_ascii_lowercase()
        }
    };
    // Summary output column names must differ in more than case, even for distinct input fields.
    let pair_field = |name: &str| identity(name).to_ascii_lowercase();
    // Index saved selections once per frame, rather than scanning all rows for each offered field.
    let mut used_names: HashMap<String, usize> = HashMap::new();
    let mut used_pairs: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for row in &rows {
        let name = if statistics.is_some() { row.get("field").and_then(Value::as_str) } else { row.as_str() }.unwrap_or_default();
        *used_names.entry(identity(name)).or_default() += 1;
        if statistics == Some(StatisticList::Summary)
            && let Some(stat) = row.get("statistic").and_then(Value::as_str).and_then(|s| StatisticList::Summary.canonical_name(s))
        {
            *used_pairs.entry(pair_field(name)).or_default().entry(stat).or_default() += 1;
        }
    }
    let mut choice_cache: Vec<(Option<FieldType>, Vec<String>)> = Vec::new();
    let count = rows.len();
    for i in 0..count {
        ui.push_id(i, |ui| {
            ui.group(|ui| {
                let Some(row) = rows.get(i) else { return };
                let mut name = match statistics {
                    None => row.as_str(),
                    Some(_) => row.get("field").and_then(Value::as_str),
                }
                .unwrap_or_default()
                .to_string();
                let old_name = name.clone();
                let selected = identity(&old_name);
                checkpoint |= field(ui, &mut name, fields, &selected, statistics.is_some(), |f| {
                    // Only called for an open picker. Summary allows several statistics per field.
                    statistics == Some(StatisticList::Summary)
                        || used_names.get(&identity(&f.name)).copied().unwrap_or(0) <= usize::from(selected == identity(&f.name))
                });
                if fields.is_some() && !name.is_empty() && resolve(&name).is_none() {
                    ui.label(tl!("Field is missing from this input."));
                }
                let old_pair = row
                    .get("statistic")
                    .and_then(Value::as_str)
                    .and_then(|s| StatisticList::Summary.canonical_name(s))
                    .map(|stat| (pair_field(&old_name), stat));
                let Some(row) = rows.get_mut(i) else { return };
                if name != old_name {
                    changed = true;
                    checkpoint |= fields.is_some();
                    if let Some(obj) = row.as_object_mut() {
                        obj.insert("field".into(), json!(name));
                    } else {
                        *row = json!(name);
                    }
                }
                if let Some(s) = statistics {
                    let ty = resolve(&name).map(|f| f.ty);
                    if !choice_cache.iter().any(|(cached, _)| *cached == ty) {
                        choice_cache.push((ty, s.choices(ty)));
                    }
                    let normalized_name = pair_field(&name);
                    let choices: Vec<&String> = choice_cache
                        .iter()
                        .find(|(cached, _)| *cached == ty)
                        .into_iter()
                        .flat_map(|(_, choices)| choices)
                        .filter(|stat| {
                            if s != StatisticList::Summary {
                                return true;
                            }
                            let used = used_pairs.get(&normalized_name).and_then(|stats| stats.get(stat.as_str())).copied().unwrap_or(0);
                            let current = old_pair.as_ref().is_some_and(|(field, selected)| field == &normalized_name && selected == *stat);
                            used <= usize::from(current)
                        })
                        .collect();
                    let mut stat = row.get(s.key()).and_then(Value::as_str).unwrap_or_default().to_string();
                    let before = stat.clone();
                    let picked = s.canonical_name(&stat);
                    let label = ui.label(if s == StatisticList::Merge { tl!("Rule") } else { tl!("Statistic") });
                    egui::ComboBox::from_id_salt("statistic")
                        .selected_text(if stat.is_empty() { "—" } else { tl!(stat.as_str()) })
                        .width((ui.available_width() - 8.0).max(40.0))
                        .show_ui(ui, |ui| {
                            for &c in &choices {
                                if ui.selectable_label(picked.as_ref() == Some(c), tl!(c)).clicked() {
                                    stat = c.clone();
                                    ui.close();
                                }
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    if !stat.is_empty() && !choices.iter().any(|choice| picked.as_ref() == Some(*choice)) {
                        ui.label(if s == StatisticList::Merge {
                            tl!("Rule is not available for this field.")
                        } else if choice_cache
                            .iter()
                            .any(|(cached, available)| *cached == ty && picked.as_ref().is_some_and(|p| available.contains(p)))
                        {
                            tl!("Statistic is already listed for this field.")
                        } else {
                            tl!("Statistic is not available for this field.")
                        });
                    }
                    if let Some(obj) = row.as_object_mut() {
                        if stat != before {
                            obj.insert(s.key().into(), json!(stat));
                            changed = true;
                            checkpoint = true;
                        }
                        if s == StatisticList::Merge && stat.trim().eq_ignore_ascii_case("join") {
                            let mut delimiter = obj.get("delimiter").and_then(Value::as_str).unwrap_or(", ").to_string();
                            let label = ui.label(tl!("Delimiter"));
                            let response =
                                ui.add(egui::TextEdit::singleline(&mut delimiter).desired_width(f32::INFINITY)).labelled_by(label.id);
                            checkpoint |= response.gained_focus();
                            if response.changed() {
                                obj.insert("delimiter".into(), json!(delimiter));
                                changed = true;
                            }
                        }
                    }
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new(tl!("Move up"))).clicked() {
                        action = Some((i, Some(i - 1)));
                    }
                    if ui.add_enabled(i + 1 < count, egui::Button::new(tl!("Move down"))).clicked() {
                        action = Some((i, Some(i + 1)));
                    }
                    if ui.button(tl!("Remove")).clicked() {
                        action = Some((i, None));
                    }
                });
            });
        });
    }
    if let Some((i, to)) = action
        && i < rows.len()
        && to.is_none_or(|to| to < rows.len())
    {
        if let Some(to) = to {
            rows.swap(i, to);
        } else {
            rows.remove(i);
        }
        changed = true;
        checkpoint = true;
    }
    if ui
        .add_enabled(
            rows.len() < 256,
            egui::Button::new(match statistics {
                Some(StatisticList::Merge) => tl!("Add rule"),
                Some(StatisticList::Summary) => tl!("Add statistic"),
                None => tl!("Add field"),
            }),
        )
        .clicked()
    {
        rows.push(match statistics {
            Some(s) => {
                let mut obj = serde_json::Map::new();
                obj.insert("field".into(), json!(""));
                obj.insert(s.key().into(), json!(""));
                Value::Object(obj)
            }
            None => json!(""),
        });
        changed = true;
        checkpoint = true;
    }
    Edit { value: changed.then_some(Value::Array(rows)), checkpoint }
}

/// Borrows and filters known field names only while the picker is open. For a future model input,
/// accepts typed names and reports focus as the start of an undo gesture.
fn field(ui: &mut Ui, name: &mut String, fields: Option<&[Field]>, selected: &str, exact: bool, offered: impl Fn(&Field) -> bool) -> bool {
    let label = ui.label(tl!("Field"));
    match fields {
        Some(fields) => {
            egui::ComboBox::from_id_salt("field")
                .selected_text(if name.is_empty() { "—" } else { name.as_str() })
                .width((ui.available_width() - 8.0).max(40.0))
                .show_ui(ui, |ui| {
                    for f in fields.iter().filter(|f| offered(f)) {
                        let n = &f.name;
                        if ui.selectable_label(if exact { selected == n } else { selected.eq_ignore_ascii_case(n) }, n).clicked() {
                            *name = n.clone();
                            ui.close();
                        }
                    }
                })
                .response
                .labelled_by(label.id);
            false
        }
        None => ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).labelled_by(label.id).gained_focus(),
    }
}
