use egui::{Key, accesskit::Role, pos2, vec2};
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use rustgis_engine::Tool;
use serde_json::{Value, json};

use super::{big_split_menu, menu_item};
use crate::RustGisApp;

#[derive(Clone, Copy)]
struct Options {
    zoom_main: bool,
    menu_enabled: bool,
    ui_enabled: bool,
    second_button: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { zoom_main: false, menu_enabled: true, ui_enabled: true, second_button: false }
    }
}

/// Draws only the controls: no app.logic, map frame, networking, or global Escape shortcut.
fn harness(options: Options) -> Harness<'static, RustGisApp> {
    let mut app = RustGisApp::with_sample();
    app.run("tool.set", json!({"tool": "measure"})).unwrap();
    let mut fonts = false;
    let mut h = Harness::builder().with_size(vec2(700.0, 420.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            ui.add_enabled_ui(options.ui_enabled, |ui| {
                ui.horizontal(|ui| {
                    let checked = app.session.tool == Tool::Select;
                    big_split_menu(
                        ui,
                        app,
                        "select",
                        if options.zoom_main { "Zoom To" } else { "Select" },
                        "Click or drag a box to select features.",
                        if options.zoom_main { "select.zoomTo" } else { "tool.set" },
                        if options.zoom_main { Value::Null } else { json!({"tool": "select"}) },
                        checked,
                        "Select options",
                        "Choose another map tool.",
                        options.menu_enabled,
                        |ui, app| {
                            menu_item(ui, app, "explore", "Explore", "tool.set", json!({"tool": "explore"}));
                            menu_item(ui, app, "measure", "Measure Distance", "tool.set", json!({"tool": "measure"}));
                        },
                    );
                    if options.second_button {
                        // Identical main command, icon and label must not share a popup identity.
                        let checked = app.session.tool == Tool::Select;
                        big_split_menu(
                            ui,
                            app,
                            "select",
                            "Select",
                            "Click or drag a box to select features.",
                            "tool.set",
                            json!({"tool": "select"}),
                            checked,
                            "Other select options",
                            "Choose another map tool.",
                            true,
                            |ui, app| {
                                menu_item(ui, app, "select", "Other Select", "tool.set", json!({"tool": "select"}));
                            },
                        );
                    }
                });
            });
        },
        app,
    );
    h.run();
    h
}

fn assert_menu(h: &Harness<'_, RustGisApp>, visible: bool) {
    assert_eq!(egui::Popup::is_any_open(&h.ctx), visible);
}

/// The popup opens immediately below the arrow in this fixed-size widget harness.
fn click_menu_row(h: &mut Harness<'_, RustGisApp>, anchor: &str, row: usize) {
    let rect = h.get_by_label(anchor).rect();
    let at = pos2(rect.min.x + 42.0, rect.max.y + 11.0 + row as f32 * 24.0);
    h.hover_at(at);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    h.run();
}

#[test]
fn primary_and_arrow_are_independent_and_alternatives_never_replace_the_default() {
    for (label, tool) in [("Explore", Tool::Explore), ("Measure Distance", Tool::Measure)] {
        let mut h = harness(Options::default());
        assert_eq!(h.get_by_label("Select").accesskit_node().role(), Role::Button);
        assert_eq!(h.get_by_label("Select options").accesskit_node().role(), Role::Button);
        assert_menu(&h, false);

        h.get_by_label("Select").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Select);
        assert_menu(&h, false);

        h.get_by_label("Select options").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Select, "the arrow must not run the primary command");
        assert_menu(&h, true);
        click_menu_row(&mut h, "Select options", if label == "Measure Distance" { 1 } else { 0 });
        assert_eq!(h.state().session.tool, tool);
        assert_menu(&h, false);

        h.get_by_label("Select").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Select, "the primary action stays Select after either alternative");
        assert_menu(&h, false);
    }
}

#[test]
fn escape_and_outside_click_dismiss_without_running_an_action() {
    for escape in [true, false] {
        let mut h = harness(Options::default());
        h.get_by_label("Select options").click();
        h.run();
        assert_menu(&h, true);
        assert_eq!(h.state().session.tool, Tool::Measure);
        if escape {
            h.key_press(Key::Escape);
        } else {
            let at = pos2(650.0, 380.0);
            h.hover_at(at);
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        h.run();
        assert_menu(&h, false);
        assert_eq!(h.state().session.tool, Tool::Measure, "dismissal must not select a tool");
    }
}

#[test]
fn tab_reaches_each_region_and_enter_or_space_activates_it() {
    for key in [Key::Enter, Key::Space] {
        let mut h = harness(Options::default());
        h.key_press(Key::Tab);
        h.run();
        assert!(h.get_by_label("Select").is_focused());
        h.key_press(key);
        h.run();
        assert_eq!(h.state().session.tool, Tool::Select);
        assert_menu(&h, false);

        h.state_mut().run("tool.set", json!({"tool": "measure"})).unwrap();
        h.key_press(Key::Tab);
        h.run();
        assert!(h.get_by_label("Select options").is_focused());
        h.key_press(key);
        h.run();
        assert_menu(&h, true);
        assert_eq!(h.state().session.tool, Tool::Measure, "keyboard activation of the arrow only opens its menu");
        click_menu_row(&mut h, "Select options", 0);
        assert_eq!(h.state().session.tool, Tool::Explore);
        assert_menu(&h, false);
    }
}

#[test]
fn disabled_primary_still_allows_enabled_menu_actions() {
    let mut h = harness(Options { zoom_main: true, ..Options::default() });
    assert!(h.state().session.map().selection_extent().is_empty());
    assert!(h.get_by_label("Zoom To").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Select options").accesskit_node().is_disabled());
    let resolution = h.state().session.map().camera.res;
    h.get_by_label("Zoom To").click();
    h.run();
    h.get_by_label("Zoom To").hover();
    h.run();
    assert!(h.query_all_by_label_contains("Unavailable").next().is_some(), "disabled primary still explains why it is unavailable");
    assert_eq!(h.state().session.map().camera.res, resolution);
    assert_eq!(h.state().session.tool, Tool::Measure);
    assert_menu(&h, false);

    // The disabled command is skipped by Tab; its arrow remains a keyboard target.
    h.remove_cursor();
    h.key_press(Key::Tab);
    h.run();
    assert!(h.get_by_label("Select options").is_focused());
    h.key_press(Key::Enter);
    h.run();
    assert_menu(&h, true);
    click_menu_row(&mut h, "Select options", 0);
    assert_eq!(h.state().session.tool, Tool::Explore);
    assert_eq!(h.state().session.map().camera.res, resolution);
    assert_menu(&h, false);
}

#[test]
fn disabled_arrow_does_not_disable_the_primary_action() {
    let mut h = harness(Options { menu_enabled: false, ..Options::default() });
    assert!(!h.get_by_label("Select").accesskit_node().is_disabled());
    assert!(h.get_by_label("Select options").accesskit_node().is_disabled());
    h.get_by_label("Select options").click();
    h.run();
    assert_menu(&h, false);
    assert_eq!(h.state().session.tool, Tool::Measure);
    h.get_by_label("Select").click();
    h.run();
    assert_eq!(h.state().session.tool, Tool::Select);
    assert_menu(&h, false);
}

#[test]
fn enclosing_disabled_ui_disables_both_regions() {
    let mut h = harness(Options { ui_enabled: false, ..Options::default() });
    for label in ["Select", "Select options"] {
        assert!(h.get_by_label(label).accesskit_node().is_disabled());
        h.get_by_label(label).click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Measure);
        assert_menu(&h, false);
    }
}

#[test]
fn identical_primary_commands_keep_distinct_popup_contents() {
    let mut h = harness(Options { second_button: true, ..Options::default() });
    h.get_by_label("Select options").click();
    h.run();
    assert_menu(&h, true);
    h.key_press(Key::Escape);
    h.run();
    assert_menu(&h, false);

    h.get_by_label("Other select options").click();
    h.run();
    assert_menu(&h, true);
    assert_eq!(h.state().session.tool, Tool::Measure);
    click_menu_row(&mut h, "Other select options", 0);
    assert_eq!(h.state().session.tool, Tool::Select);
}
