//! Image attributes: their thumbnails in the attribute table and the identify popup, and the
//! viewer that shows the original. The bytes stay in the source and are read on demand, off the
//! frame on the desktop; the project and the dataset hold only metadata and a thumbnail.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use rustgis_engine::data::pictures;
use rustgis_engine::map::source::ImageJob;

/// The most thumbnails kept before the oldest goes.
const THUMBS: usize = 240;
/// The most pictures kept for the assistant's transcript before the oldest goes.
const PREVIEWS: usize = 24;
/// The longest side the viewer decodes an image at.
const VIEWER_SIDE: u32 = 4096;

/// The layer, feature, field and image size a thumbnail belongs to: a row whose image changed
/// size reads again.
pub type Key = (u32, u64, String, u64);

/// Thumbnails and the open viewer.
pub struct Images {
    thumbs: HashMap<Key, Thumb>,
    /// The order thumbnails were added in, oldest first (the LRU's eviction order).
    order: VecDeque<Key>,
    /// The channel read requests land on (desktop only: the browser reads them on the frame).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    tx: Sender<Done>,
    rx: Receiver<Done>,
    viewer: Option<Viewer>,
    seq: u64,
    /// Pictures the assistant's transcript shows, by hash of their `data:` address.
    previews: HashMap<u64, TextureHandle>,
    /// The order previews were added in, oldest first (the LRU's eviction order).
    preview_order: VecDeque<u64>,
}

enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed(String),
}

/// What a cell shows of an image attribute.
pub enum ThumbView {
    Loading,
    Ready(TextureHandle),
    Failed(String),
    /// No source behind it (a table copied from one, say): there's nothing to read.
    Unavailable,
}

struct Done {
    key: Key,
    kind: Kind,
    result: Result<ColorImage, String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Thumb,
    View(u64),
}

struct Viewer {
    /// Tells this request from one the viewer made before (results land in order).
    seq: u64,
    title: String,
    image: ViewerImage,
    size: Option<(u32, u32)>,
    zoom: f32,
}

enum ViewerImage {
    Loading,
    Ready(TextureHandle),
    Failed(String),
}

impl Default for Images {
    fn default() -> Self {
        let (tx, rx) = channel();
        Images {
            thumbs: HashMap::new(),
            order: VecDeque::new(),
            tx,
            rx,
            viewer: None,
            seq: 0,
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
        }
    }
}

impl Images {
    /// A thumbnail for an image attribute, requested if it isn't kept yet.
    pub fn thumb(&mut self, ctx: &Context, key: Key, job: Option<ImageJob>) -> ThumbView {
        if !self.thumbs.contains_key(&key)
            && let Some(job) = job
        {
            self.thumbs.insert(key.clone(), Thumb::Loading);
            self.order.push_back(key.clone());
            self.fetch(ctx, key.clone(), job, pictures::THUMB_SIZE, Kind::Thumb);
        }
        match self.thumbs.get(&key) {
            Some(Thumb::Ready(t)) => ThumbView::Ready(t.clone()),
            Some(Thumb::Failed(e)) => ThumbView::Failed(e.clone()),
            Some(Thumb::Loading) => ThumbView::Loading,
            None => ThumbView::Unavailable,
        }
    }

    /// A picture kept as a `data:` address (the thumbnail of one the assistant read), decoded once
    /// for the transcript and kept while it shows. `None` when the address isn't a picture.
    pub fn preview(&mut self, ctx: &Context, data: &str) -> Option<TextureHandle> {
        use base64::Engine;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut h);
        let key = h.finish();
        if let Some(tex) = self.previews.get(&key) {
            return Some(tex.clone());
        }
        let encoded = data.split_once(',').map(|(_, d)| d)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
        let img = pictures::decode(&bytes, pictures::THUMB_SIZE).ok()?;
        let color = ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        let tex = ctx.load_texture("assistant-picture", color, TextureOptions::LINEAR);
        if self.previews.len() >= PREVIEWS
            && let Some(old) = self.preview_order.pop_front()
        {
            self.previews.remove(&old);
        }
        self.previews.insert(key, tex.clone());
        self.preview_order.push_back(key);
        Some(tex)
    }

    /// Opens the viewer on an image attribute, reading the original.
    pub fn open(&mut self, ctx: &Context, key: Key, title: String, job: Option<ImageJob>) {
        self.seq += 1;
        let seq = self.seq;
        let mut viewer = Viewer { seq, title, image: ViewerImage::Loading, size: None, zoom: 1.0 };
        match job {
            Some(job) => self.fetch(ctx, key, job, VIEWER_SIDE, Kind::View(seq)),
            None => viewer.image = ViewerImage::Failed(crate::tl!("the image isn't read from a data source of this layer").to_string()),
        }
        self.viewer = Some(viewer);
    }

    /// Takes the thumbnails and images that landed since the last frame.
    pub fn poll(&mut self, ctx: &Context) {
        let mut done = Vec::new();
        while let Ok(d) = self.rx.try_recv() {
            done.push(d);
        }
        for d in done {
            self.done(ctx, d);
        }
    }

    fn fetch(&mut self, ctx: &Context, key: Key, job: ImageJob, max_side: u32, kind: Kind) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            let done_key = key.clone();
            let spawned = std::thread::Builder::new().name("image-attribute".into()).spawn(move || {
                let result = read(&job, done_key.1, &done_key.2, max_side);
                let _ = tx.send(Done { key: done_key, kind, result });
                ctx.request_repaint();
            });
            if spawned.is_err() {
                self.fail(key, kind, "couldn't start reading the image".into());
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let result = read(&job, key.1, &key.2, max_side);
            self.done(ctx, Done { key, kind, result });
        }
    }

    fn done(&mut self, ctx: &Context, d: Done) {
        match d.kind {
            Kind::Thumb => {
                if self.thumbs.contains_key(&d.key) {
                    let thumb = match d.result {
                        Ok(img) => Thumb::Ready(self.load(ctx, &d.key, "image-thumb", img)),
                        Err(e) => Thumb::Failed(e),
                    };
                    self.thumbs.insert(d.key, thumb);
                    self.prune();
                }
            }
            Kind::View(seq) => {
                if self.viewer.as_ref().is_none_or(|v| v.seq != seq) {
                    return;
                }
                match d.result {
                    Ok(img) => {
                        let size = Some((img.width() as u32, img.height() as u32));
                        let tex = self.load(ctx, &d.key, "image-view", img);
                        if let Some(viewer) = &mut self.viewer {
                            viewer.size = size;
                            viewer.image = ViewerImage::Ready(tex);
                        }
                    }
                    Err(e) => {
                        if let Some(viewer) = &mut self.viewer {
                            viewer.image = ViewerImage::Failed(e);
                        }
                    }
                }
            }
        }
    }

    /// Records a request that couldn't even be started (desktop only).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn fail(&mut self, key: Key, kind: Kind, message: String) {
        match kind {
            Kind::Thumb => {
                self.thumbs.insert(key, Thumb::Failed(message));
            }
            Kind::View(seq) => {
                if let Some(v) = &mut self.viewer
                    && v.seq == seq
                {
                    v.image = ViewerImage::Failed(message);
                }
            }
        }
    }

    fn load(&self, ctx: &Context, key: &Key, name: &str, image: ColorImage) -> TextureHandle {
        let name = format!("{name}-{}-{}-{}", key.0, key.1, key.2);
        ctx.load_texture(name, image, TextureOptions::LINEAR)
    }

    /// Evicts the oldest thumbnails beyond the cache's size.
    fn prune(&mut self) {
        while self.thumbs.len() > THUMBS {
            let Some(old) = self.order.pop_front() else { break };
            self.thumbs.remove(&old);
        }
    }
}

/// The image's decoded pixels; the format is checked from its header first by the decoder.
fn read(job: &ImageJob, fid: u64, field: &str, max_side: u32) -> Result<ColorImage, String> {
    let bytes = job.image(fid, field)?;
    if bytes.len() > pictures::MAX_BYTES {
        return Err(crate::tl!("the image is too large to show").to_string());
    }
    let img = pictures::decode(&bytes, max_side).map_err(|e| e.to_string())?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    Ok(ColorImage::from_rgba_unmultiplied([w, h], img.as_raw()))
}

/// The viewer window, while one is open.
pub fn viewer(images: &mut Images, ctx: &Context) {
    let Some(v) = &mut images.viewer else { return };
    let title = v.title.clone();
    let mut open = true;
    let mut zoom = v.zoom;
    egui::Window::new(title).id(egui::Id::unique("image_viewer")).open(&mut open).default_size([720.0, 560.0]).resizable(true).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                let size = match v.size {
                    Some((w, h)) => format!("{w} × {h}"),
                    None => String::new(),
                };
                ui.label(egui::RichText::new(size).size(11.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("+")).on_hover_text("Zoom in").clicked() {
                        zoom = (zoom * 1.25).min(8.0);
                    }
                    let pct = format!("{:.0}%", zoom * 100.0);
                    ui.label(egui::RichText::new(pct).size(11.5));
                    if ui.add(egui::Button::new("−")).on_hover_text("Zoom out").clicked() {
                        zoom = (zoom / 1.25).max(0.05);
                    }
                    if ui.add(egui::Button::new("1:1")).on_hover_text("Actual size").clicked() {
                        zoom = 1.0;
                    }
                });
            });
            ui.separator();
            match &v.image {
                ViewerImage::Loading => {
                    ui.add(egui::Spinner::new());
                }
                ViewerImage::Failed(e) => {
                    ui.label(egui::RichText::new(e.as_str()).size(12.0));
                }
                ViewerImage::Ready(tex) => {
                    let size = tex.size_vec2() * zoom;
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        ui.add(egui::Image::new((tex.id(), size)).corner_radius(4));
                    });
                }
            }
        },
    );
    v.zoom = zoom;
    if !open {
        images.viewer = None;
    }
}
