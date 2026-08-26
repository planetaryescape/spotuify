//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_mosaic.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::rng_next;
use super::{put, Ctx, StepClock};

/// Characters per tile, and the gap between tiles.
const TILE_WIDTH: usize = 2;
const TILE_GAP: usize = 1;
/// Per-step decay of a tile's brightness. Slow enough that a tile lingers for
/// a beat, fast enough that a passage's shape is still legible.
const DECAY: f32 = 0.88;
/// Below this a tile is considered dark, so it stops being repainted.
const DARK: f32 = 0.001;
/// Ignition thresholds are drawn from this range. The spread is what makes
/// lit-tile density rise gradually with loudness instead of all-or-nothing.
const MIN_THRESHOLD: f32 = 0.04;
const THRESHOLD_SPAN: f32 = 0.74;
/// Cap on ignition brightness, above the top tier so a spike can briefly
/// promote a tile past merely "full".
const MAX_IGNITION: f32 = 1.05;
/// `(threshold, glyph, tier)` from brightest down. First match wins.
const LEVELS: [(f32, char, u8); 6] = [
    (0.85, '█', 2),
    (0.65, '█', 1),
    (0.45, '█', 0),
    (0.28, '▓', 0),
    (0.15, '▒', 0),
    (0.05, '░', 0),
];

/// One tile: which band it listens to, how loud that band must be to light
/// it, and how lit it currently is.
#[derive(Clone, Copy, Debug, Default)]
struct Tile {
    band: usize,
    threshold: f32,
    value: f32,
}

/// A fixed grid of heatmap tiles. Nothing scrolls; tiles ignite and fade
/// where they sit.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    tiles: Vec<Tile>,
    rows: usize,
    columns: usize,
    /// Band count the tiles were wired against. Part of the key: a different
    /// spectrum width means every tile's band index is stale.
    bands: usize,
    rebuilds: u32,
}

impl State {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    pub(super) fn is_primed(&self) -> bool {
        !self.tiles.is_empty()
    }

    /// Wire every tile to a band biased by its row (bass at the bottom,
    /// treble at the top) plus a small jitter, and give it its own ignition
    /// threshold.
    fn ensure(&mut self, rows: usize, columns: usize, bands: usize) -> bool {
        if self.rows == rows && self.columns == columns && self.bands == bands {
            return false;
        }
        self.rows = rows;
        self.columns = columns;
        self.bands = bands;
        self.rebuilds = self.rebuilds.saturating_add(1);

        let mut rng = 0xC1AB_1A10_15D5_u64;
        self.tiles = Vec::with_capacity(rows * columns);
        for row in 0..rows {
            let base = if rows > 1 {
                (rows - 1 - row) * (bands - 1) / (rows - 1)
            } else {
                bands / 2
            };
            for _ in 0..columns {
                let jitter = (rng_next(&mut rng) * 5.0) as usize;
                let band = (base + jitter).saturating_sub(2).min(bands - 1);
                self.tiles.push(Tile {
                    band,
                    threshold: MIN_THRESHOLD + rng_next(&mut rng) * THRESHOLD_SPAN,
                    value: 0.0,
                });
            }
        }
        true
    }
}

/// How many tiles fit across `width`. The last tile needs no trailing gap.
fn tile_count(width: u16) -> usize {
    let width = usize::from(width);
    if width < TILE_WIDTH {
        return 0;
    }
    (width + TILE_GAP) / (TILE_WIDTH + TILE_GAP)
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.anim_frame());
    let (rows, columns) = (usize::from(area.height), tile_count(area.width));
    if rows == 0 || columns == 0 || ctx.bands.is_empty() {
        return;
    }
    let steps = if state.ensure(rows, columns, ctx.bands.len()) {
        steps.max(1)
    } else {
        steps
    };

    for _ in 0..steps {
        for tile in &mut state.tiles {
            let level = ctx.bands[tile.band];
            if level > tile.threshold {
                tile.value = tile.value.max(level.min(MAX_IGNITION));
            }
            tile.value *= DECAY;
            if tile.value < DARK {
                tile.value = 0.0;
            }
        }
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let columns = tile_count(area.width);
    if state.rows != usize::from(area.height) || state.columns != columns || columns == 0 {
        return;
    }

    for row in 0..area.height {
        for column in 0..columns {
            let value = state.tiles[usize::from(row) * columns + column].value;
            let Some((_, glyph, tier)) = LEVELS.iter().find(|(floor, _, _)| value >= *floor) else {
                continue;
            };
            let style = ctx.paint.tier(*tier);
            let left = column * (TILE_WIDTH + TILE_GAP);
            for cell in 0..TILE_WIDTH {
                put(buf, area, (left + cell) as u16, row, *glyph, style);
            }
        }
    }
}
