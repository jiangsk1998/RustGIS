//! Sign in to ArcGIS, in Add From Service: an ArcGIS Online organization or an ArcGIS Enterprise
//! portal, through its own sign-in page in the browser (`arcgis.signIn`), so its secured services
//! can be added. Who is signed in, and Sign out (`arcgis.signOut`). The portal's address and the
//! OAuth client id are remembered; tokens never are (see `rustgis_tiles::arcgis_auth`).

use crate::RustGisApp;
use crate::theme::Tokens;
use rustgis_engine::render::rustgis_tiles::arcgis_auth;
use serde_json::json;

const PORTAL_PREF: &str = "arcgis_portal";
const CLIENT_PREF: &str = "arcgis_client_id";

#[derive(Default)]
pub struct State {
    pub portal: String,
    pub client_id: String,
    /// The remembered portal and client id were read.
    loaded: bool,
    pub error: Option<String>,
    /// The browser's sign-in under way: who signed in, or why not.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pending: Option<crate::Slot<Result<String, String>>>,
}

/// Whether Add From Service shows the sign-in: the ArcGIS REST type, an ArcGIS-looking address,
/// or a session kept.
pub fn wanted(app: &RustGisApp) -> bool {
    let address = app.dialogs.text.to_ascii_lowercase();
    // A service known as ArcGIS by its answer (behind a proxy's address), or one refused for a token.
    let answered = app.ui.service.as_ref().is_some_and(|c| {
        c.listing.as_ref().and_then(|v| v.get("kind")).and_then(serde_json::Value::as_str).is_some_and(|k| k.starts_with("ArcGIS"))
            || c.error.as_deref().is_some_and(|e| e.contains("Token Required") || e.contains("ArcGIS token"))
    });
    app.dialogs.mode == "arcgis"
        || address.contains("/rest/services")
        || address.contains("arcgis")
        || answered
        || !arcgis_auth::list().is_empty()
}

/// The sign-in's controls, folded under a header.
pub fn section(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if !app.ui.arcgis.loaded {
        app.ui.arcgis.loaded = true;
        if let Some(load) = &app.services.load_pref {
            app.ui.arcgis.portal = load(PORTAL_PREF).unwrap_or_default();
            app.ui.arcgis.client_id = load(CLIENT_PREF).unwrap_or_default();
        }
    }
    let t = Tokens::get(ui.ctx());
    let sessions = arcgis_auth::list();
    egui::CollapsingHeader::new(tl!("Sign in to ArcGIS")).id_salt("arcgis_sign_in").default_open(!sessions.is_empty()).show(ui, |ui| {
        let mut sign_out = None;
        for s in &sessions {
            ui.horizontal(|ui| {
                let text = match (s.token, s.username.is_empty()) {
                    (true, _) => crate::i18n::fmt("A token for {server}", &[("server", &s.portal)]),
                    (false, true) => crate::i18n::fmt("Signed in to {portal}", &[("portal", &s.portal)]),
                    (false, false) => crate::i18n::fmt("Signed in to {portal} as {user}", &[("portal", &s.portal), ("user", &s.username)]),
                };
                ui.label(text);
                if ui.button(tl!("Sign out")).clicked() {
                    sign_out = Some(s.portal.clone());
                }
            });
        }
        if let Some(portal) = sign_out
            && let Err(e) = app.run("arcgis.signOut", json!({"portal": portal}))
        {
            app.ui.arcgis.error = Some(e);
        }
        form(app, ui, &t);
        if let Some(e) = &app.ui.arcgis.error {
            ui.colored_label(t.red, e);
        }
    });
}

/// The portal and client id fields, with what to register.
fn fields(app: &mut RustGisApp, ui: &mut egui::Ui) {
    prefill(&mut app.ui.arcgis);
    let redirect = if cfg!(target_arch = "wasm32") {
        format!("…/{}", arcgis_auth::WEB_CALLBACK)
    } else {
        arcgis_auth::redirect_uri(arcgis_auth::PORT)
    };
    ui.label(
        egui::RichText::new(crate::i18n::fmt(
            "For ArcGIS Online, the client ID is HuaGIS's own app. For an ArcGIS Enterprise portal, register an OAuth app on it (Content › New item › Developer credentials) whose redirect URL is {redirect}, and give its client ID.",
            &[("redirect", &redirect)],
        ))
        .weak(),
    );
    egui::Grid::new("arcgis_sign_in_form").num_columns(2).spacing(egui::vec2(10.0, 8.0)).show(ui, |ui| {
        let label = ui.label(tl!("Portal"));
        ui.add(egui::TextEdit::singleline(&mut app.ui.arcgis.portal).hint_text("https://myorg.maps.arcgis.com").desired_width(320.0))
            .labelled_by(label.id)
            .on_hover_text(tl!("An ArcGIS Online organization's or an ArcGIS Enterprise portal's address; ArcGIS Online when blank"));
        ui.end_row();
        let label = ui.label(tl!("Client ID"));
        ui.add(egui::TextEdit::singleline(&mut app.ui.arcgis.client_id).hint_text(tl!("The portal's OAuth app")).desired_width(320.0))
            .labelled_by(label.id);
        ui.end_row();
    });
}

/// Shows RustGIS's own client id for an ArcGIS Online portal (blank, or an organization's
/// address) when none is given, and takes it away for an Enterprise portal, which registers its own.
/// A client id the user types is kept.
fn prefill(s: &mut State) {
    let online = arcgis_auth::portal_base(&s.portal).is_ok_and(|p| arcgis_auth::is_online(&p));
    let id = s.client_id.trim();
    if online && id.is_empty() {
        s.client_id = arcgis_auth::DEFAULT_CLIENT_ID.to_string();
    } else if !online && id == arcgis_auth::DEFAULT_CLIENT_ID {
        s.client_id.clear();
    }
}

fn remember(app: &RustGisApp, portal: &str, client_id: &str) {
    if let Some(save) = &app.services.save_pref {
        save(PORTAL_PREF, portal);
        save(CLIENT_PREF, client_id);
    }
}

/// The browser: the sign-in runs in a pop-up window (`Services::arcgis_sign_in`).
#[cfg(target_arch = "wasm32")]
fn form(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    fields(app, ui);
    let done = app.ui.arcgis.pending.as_ref().and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).take());
    if let Some(result) = done {
        app.ui.arcgis.pending = None;
        app.ui.arcgis.error = result.err();
    }
    if app.ui.arcgis.pending.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Finish signing in in the pop-up window…"));
        });
        return;
    }
    if crate::widgets::dialog_button(ui, tl!("Sign in"), Some(t.accent), egui::vec2(96.0, 28.0)).clicked() {
        let (portal, client_id) = (app.ui.arcgis.portal.trim().to_string(), app.ui.arcgis.client_id.trim().to_string());
        app.ui.arcgis.error = None;
        if let Some(sign_in) = &app.services.arcgis_sign_in {
            let slot = crate::Slot::default();
            sign_in(portal.clone(), client_id.clone(), slot.clone());
            app.ui.arcgis.pending = Some(slot);
            remember(app, &portal, &client_id);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn form(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    use arcgis_auth::Progress;
    fields(app, ui);
    match arcgis_auth::progress() {
        Progress::Pending { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Finish signing in in the browser…"));
                if ui.button(tl!("Cancel")).clicked() {
                    let _ = app.run("arcgis.cancelSignIn", json!({}));
                }
            });
            // The browser comes back on another thread: look again soon.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        progress => {
            if let Progress::Failed(e) = &progress {
                ui.colored_label(t.red, e);
            }
            if crate::widgets::dialog_button(ui, tl!("Sign in"), Some(t.accent), egui::vec2(96.0, 28.0)).clicked() {
                start(app);
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start(app: &mut RustGisApp) {
    app.ui.arcgis.error = None;
    let (portal, client_id) = (app.ui.arcgis.portal.trim().to_string(), app.ui.arcgis.client_id.trim().to_string());
    match app.run("arcgis.signIn", json!({"portal": portal, "clientId": client_id})) {
        Ok(v) => {
            remember(app, &portal, &client_id);
            app.ui.open_url = v.get("url").and_then(serde_json::Value::as_str).map(str::to_string);
        }
        Err(e) => app.ui.arcgis.error = Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcgis_online_shows_the_default_client_id() {
        let mut s = State::default();
        prefill(&mut s);
        assert_eq!(s.client_id, arcgis_auth::DEFAULT_CLIENT_ID);
        s.portal = "https://myorg.maps.arcgis.com".into();
        prefill(&mut s);
        assert_eq!(s.client_id, arcgis_auth::DEFAULT_CLIENT_ID);
        // An Enterprise portal registers its own.
        s.portal = "https://gis.example.org/portal".into();
        prefill(&mut s);
        assert!(s.client_id.is_empty());
        // One typed is kept, whichever the portal.
        s.client_id = "mine".into();
        s.portal.clear();
        prefill(&mut s);
        assert_eq!(s.client_id, "mine");
    }
}
