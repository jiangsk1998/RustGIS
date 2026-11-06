//! Screen snapshots for the MCP relay: an attached agent's `snapshot` sees the map exactly as
//! the user does. The relay holds the request ([`rustgis_assistant::relay::Relay::screen_request`]);
//! this waits for the map to finish drawing (at most the request's wait), asks egui for a
//! screenshot of the window, crops it to the map view and hands the PNG back.

use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, Rect};

use crate::RustGisApp;

/// A screenshot not back this long after the wait is given up, and the map drawn offscreen.
const SCREENSHOT_GRACE: f64 = 10.0;

/// The next screenshot's number. Each screenshot this asks egui for is numbered, so only that
/// snapshot's screenshot is taken for it (not a late one of an earlier snapshot that timed out).
static NEXT_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// An app's slot for the screenshot it waits for: that screenshot's number (0 for none) and the
/// screenshot once egui hands it back. Only that one is kept: one coming back after its snapshot
/// ended is dropped.
#[derive(Debug, Clone, Default)]
pub struct Arrived(Arc<Mutex<(u64, Option<Arc<ColorImage>>)>>);

impl Arrived {
    /// Waits for the screenshot numbered `tag` (0: for none), dropping any other.
    fn wait_for(&self, tag: u64) {
        if let Ok(mut arrived) = self.0.lock() {
            *arrived = (tag, None);
        }
    }

    /// The screenshot numbered `tag`, if it is back.
    fn take(&self, tag: u64) -> Option<Arc<ColorImage>> {
        let mut arrived = self.0.lock().ok()?;
        if arrived.0 == tag { arrived.1.take() } else { None }
    }

    /// Keeps `image` if it is the screenshot numbered `tag` waited for. One dropped (a late one, or
    /// a poisoned lock) is safe: the snapshot waits out its grace and draws the map offscreen.
    fn arrive(&self, tag: u64, image: Arc<ColorImage>) {
        if let Ok(mut arrived) = self.0.lock()
            && arrived.0 == tag
        {
            arrived.1 = Some(image);
        }
    }
}

/// A screen snapshot under way.
#[derive(Debug, Clone, Copy)]
pub struct Shot {
    /// When it started (egui's input time, seconds).
    started: f64,
    /// The longest to wait for the map to finish drawing (seconds).
    wait: f64,
    /// When the screenshot was asked for: whether the map had finished drawing, and where the
    /// map was on screen at what scale, which the crop uses whatever moves meanwhile.
    asked: Option<(bool, Rect, f32)>,
    /// The number its screenshot is tagged with, once asked for.
    tag: u64,
}

/// Whether the map view was drawn on the last frame, so a snapshot can capture it.
pub fn map_shown(app: &RustGisApp, ctx: &egui::Context) -> bool {
    view_rect(app, ctx).is_some()
}

/// The map view's place on screen, if it was drawn on the last frame or this one.
fn view_rect(app: &RustGisApp, ctx: &egui::Context) -> Option<Rect> {
    let frame = ctx.cumulative_frame_nr();
    app.map.view_rect.filter(|(r, at)| frame.saturating_sub(*at) <= 1 && r.is_positive()).map(|(r, _)| r)
}

/// Whether the map has finished drawing: no tiles on the way and no tilted frame being drawn.
fn settled(app: &RustGisApp) -> bool {
    rustgis_engine::render::rustgis_tiles::cache::in_flight() == 0 && !app.map.drawing()
}

/// Drives the relay's screen snapshot, if one is waiting: called every frame after the relay
/// ran its messages.
pub fn drive(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(relay) = app.relay.clone() else {
        if app.relay_shot.take().is_some() {
            app.relay_arrived.wait_for(0);
        }
        app.map.hide_overlays = false;
        return;
    };
    let Some(request) = relay.screen_request() else {
        if app.relay_shot.take().is_some() {
            app.relay_arrived.wait_for(0);
        }
        app.map.hide_overlays = false;
        return;
    };
    let now = ctx.input(|i| i.time);
    let shot = *app.relay_shot.get_or_insert(Shot { started: now, wait: request.wait.as_secs_f64(), asked: None, tag: 0 });
    let rect = view_rect(app, ctx);
    let finish = |app: &mut RustGisApp, png: Result<Vec<u8>, String>, settled: bool| {
        app.relay_shot = None;
        app.relay_arrived.wait_for(0);
        app.map.hide_overlays = false;
        relay.finish_screen(&mut app.session, png, settled);
    };
    match shot.asked {
        Some((was_settled, at, ppp)) => {
            if let Some(image) = app.relay_arrived.take(shot.tag) {
                let png = crop_png(&image, at, ppp)
                    .ok_or_else(|| "the window's screenshot didn't hold the map, so it was drawn offscreen".to_string());
                return finish(app, png, was_settled);
            }
            // A backend that never answers: drawn offscreen.
            if now - shot.started > shot.wait + SCREENSHOT_GRACE {
                return finish(app, Err("the window's screenshot didn't come back in time, so the map was drawn offscreen".into()), false);
            }
        }
        None => {
            // The map went away (a layout opened): drawn offscreen instead.
            let Some(at) = rect else {
                return finish(
                    app,
                    Err("the map isn't on screen in HuaGIS (a layout or the catalog is shown), so it was drawn offscreen".into()),
                    false,
                );
            };
            let done = settled(app);
            // At least a frame after the request, so the map shows what the agent last did.
            if now > shot.started && (done || now - shot.started >= shot.wait) {
                let tag = NEXT_TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                app.relay_shot = Some(Shot { asked: Some((done, at, ctx.pixels_per_point())), tag, ..shot });
                // Overlays leave only the frame being captured (this one draws after this), and
                // come back with the screenshot.
                app.map.hide_overlays = !request.overlays;
                app.relay_arrived.wait_for(tag);
                let (arrived, repaint) = (app.relay_arrived.clone(), ctx.clone());
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                    arrived.arrive(tag, image);
                    repaint.request_repaint();
                })));
            }
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

/// The part of a window screenshot under `rect` (points), as a PNG. `None` when the rectangle
/// holds no pixels of the image.
pub fn crop_png(image: &ColorImage, rect: Rect, pixels_per_point: f32) -> Option<Vec<u8>> {
    let [w, h] = image.size;
    let px = |v: f32, max: usize| ((v * pixels_per_point).round().max(0.0) as usize).min(max);
    let (x0, x1) = (px(rect.min.x, w), px(rect.max.x, w));
    let (y0, y1) = (px(rect.min.y, h), px(rect.max.y, h));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(cw * ch * 4);
    for y in y0..y1 {
        let row = image.pixels.get(y * w + x0..y * w + x1)?;
        rgba.extend(row.iter().flat_map(|c: &Color32| c.to_srgba_unmultiplied()));
    }
    let img = image::RgbaImage::from_raw(u32::try_from(cw).ok()?, u32::try_from(ch).ok()?, rgba)?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crop takes the map's pixels at the screen's scale, and a rectangle off the image
    /// gives nothing rather than failing.
    #[test]
    fn crops_the_map_view_at_the_screen_scale() {
        let (w, h) = (40, 30);
        let mut pixels = vec![Color32::WHITE; w * h];
        // A red block at pixels (10..30, 6..18): the map at points (5..15, 3..9) at 2x.
        for y in 6..18 {
            for x in 10..30 {
                pixels[y * w + x] = Color32::RED;
            }
        }
        let image = ColorImage::new([w, h], pixels);
        let png = crop_png(&image, Rect::from_min_max(egui::pos2(5.0, 3.0), egui::pos2(15.0, 9.0)), 2.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (20, 12));
        assert!(img.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        // Partly off the image: cut to it.
        let png = crop_png(&image, Rect::from_min_max(egui::pos2(15.0, 10.0), egui::pos2(100.0, 100.0)), 2.0).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8().dimensions(), (10, 10));
        // Wholly off it, or empty.
        assert!(crop_png(&image, Rect::from_min_max(egui::pos2(50.0, 50.0), egui::pos2(60.0, 60.0)), 2.0).is_none());
        assert!(crop_png(&image, Rect::NOTHING, 1.0).is_none());
    }
}
