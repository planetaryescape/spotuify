//! Per-frame click-target registry.
//!
//! Renderers register the rects they actually draw (rows, chips) and
//! the mouse path resolves clicks with a single lookup. This replaces
//! per-screen geometry math in `app.rs` that had drifted from the
//! renderers — 1-line row heights against 2-line rendered rows,
//! ignored scroll offsets, ignored column splits — so clicks selected
//! the wrong row on most screens (and the wrong DEVICE on the devices
//! table). The renderer is the only authority on where things are; the
//! hit map just records it.
//!
//! `ui::render` clears the map at the top of every frame; renderers
//! push as they draw; `app.rs` reads it on mouse events. Single
//! threaded (all inside one `terminal.draw` + event loop), interior
//! mutability via `RefCell` on `App` because renderers take `&App`.

use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitTarget {
    /// A selectable row. `index` is in the SAME selection space the
    /// keyboard uses for that screen (`visible_items()` /
    /// `filtered_playlists()` / `filtered_devices()` …) — it feeds
    /// straight into `App::set_active_selection`.
    Row { index: usize },
    /// The right rail's hide chip ("Q hide" / "L hide").
    RailToggle,
    /// The right rail's title row outside the hide chip (expand).
    RailFullscreen,
}

#[derive(Default, Debug)]
pub(crate) struct HitMap {
    regions: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    pub(crate) fn clear(&mut self) {
        self.regions.clear();
    }

    pub(crate) fn push(&mut self, rect: Rect, target: HitTarget) {
        if rect.width > 0 && rect.height > 0 {
            self.regions.push((rect, target));
        }
    }

    /// Last registration wins — later pushes are drawn on top.
    pub(crate) fn target_at(&self, column: u16, row: u16) -> Option<HitTarget> {
        self.regions
            .iter()
            .rev()
            .find(|(rect, _)| {
                column >= rect.x
                    && column < rect.x.saturating_add(rect.width)
                    && row >= rect.y
                    && row < rect.y.saturating_add(rect.height)
            })
            .map(|(_, target)| *target)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.regions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_hits_registered_rect_and_prefers_topmost() {
        let mut map = HitMap::default();
        map.push(Rect::new(0, 0, 10, 4), HitTarget::Row { index: 0 });
        map.push(Rect::new(0, 2, 10, 2), HitTarget::Row { index: 1 });
        assert_eq!(map.target_at(5, 1), Some(HitTarget::Row { index: 0 }));
        // Overlap: the later (topmost) registration wins.
        assert_eq!(map.target_at(5, 2), Some(HitTarget::Row { index: 1 }));
        assert_eq!(map.target_at(11, 1), None);
        map.clear();
        assert_eq!(map.target_at(5, 1), None);
    }

    #[test]
    fn zero_sized_rects_are_ignored() {
        let mut map = HitMap::default();
        map.push(Rect::new(0, 0, 0, 5), HitTarget::RailToggle);
        map.push(Rect::new(0, 0, 5, 0), HitTarget::RailToggle);
        assert_eq!(map.len(), 0);
    }
}
