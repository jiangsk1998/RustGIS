//! Whole-source LOD decisions survive disposable interaction windows and render snapshots.
use rustgis_data::source::FeatureSource;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(super) struct Decisions {
    source: Option<Weak<dyn FeatureSource>>,
    // Most recently used first; a decision holds no feature or projected geometry.
    entries: Vec<(String, i32, bool)>,
}

impl Decisions {
    /// Computes only missing decisions, retaining successes for this exact open source.
    /// The caller holds the source's `render_lod` mutex throughout calculation, so concurrent
    /// render snapshots wait for a fill instead of repeating the same traversal.
    pub(super) fn get_or_try_insert(
        &mut self,
        source: &Arc<dyn FeatureSource>,
        projection: &str,
        level: i32,
        calculate: impl FnOnce() -> rustgis_data::Result<bool>,
    ) -> rustgis_data::Result<bool> {
        if !self.source.as_ref().and_then(Weak::upgrade).is_some_and(|old| Arc::ptr_eq(&old, source)) {
            self.entries.clear();
            self.source = Some(Arc::downgrade(source));
        }
        if let Some(i) = self.entries.iter().position(|(p, l, _)| p == projection && *l == level) {
            let hit = self.entries.remove(i);
            let decision = hit.2;
            self.entries.insert(0, hit);
            return Ok(decision);
        }
        let decision = calculate()?;
        self.entries.insert(0, (projection.into(), level, decision));
        self.entries.truncate(crate::lod::LEVELS_KEPT);
        Ok(decision)
    }
}
