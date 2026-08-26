//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_sand.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, rng_next, BrailleGrid};
use super::{Ctx, StepClock};

/// Band level below which a band stops emitting grains.
const EMIT_FLOOR: f32 = 0.10;
/// Emission probability per step, as a fraction of the band's level.
const EMIT_RATE: f32 = 0.85;
/// A bass rise this large, from at least this level, counts as a kick.
const KICK_DELTA: f32 = 0.06;
const KICK_FLOOR: f32 = 0.15;
/// Bed occupancy above which the next kick detonates it instead of shaking it.
const DETONATE_FILL: f32 = 0.30;
/// Bass level above which the bed keeps churning every step.
const RUMBLE_FLOOR: f32 = 0.30;
/// Chance a bottom-row grain drains off-screen per step. Without this the bed
/// packs solid over a long session and the automaton stops moving.
const DRAIN_RATE: f32 = 0.04;
/// Explosion ballistics, in dots per step.
const EXPLODE_GRAVITY: f32 = 0.50;
const EXPLODE_DRAG: f32 = 0.985;
/// Safety cap on explosion length; normally it ends when the last grain
/// leaves the panel.
const EXPLODE_MAX_STEPS: u32 = 80;

/// A grain in ballistic flight during an explosion. Sub-dot position and a
/// real velocity are what let the burst rise, peak, and fall across frames
/// instead of teleporting.
#[derive(Clone, Copy, Debug)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    tier: u8,
}

/// A falling-sand automaton on the dot grid. Grains rain from the top in
/// their band's colour, pile up, and get thrown around by bass.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    /// Tier per dot, 0 = empty. Row 0 is the top.
    grid: Vec<u8>,
    dot_rows: usize,
    dot_cols: usize,
    rng: u64,
    previous_bass: f32,
    particles: Vec<Particle>,
    explosion_steps: u32,
    /// Steps run so far; only its parity is used, to alternate the falling
    /// pass's scan direction so piles don't lean permanently one way.
    steps: u64,
    rebuilds: u32,
}

impl State {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    pub(super) fn is_primed(&self) -> bool {
        !self.grid.is_empty()
    }

    fn ensure(&mut self, dot_rows: usize, dot_cols: usize) -> bool {
        if self.dot_rows == dot_rows && self.dot_cols == dot_cols {
            return false;
        }
        self.grid = vec![0; dot_rows * dot_cols];
        self.dot_rows = dot_rows;
        self.dot_cols = dot_cols;
        self.rng = 0x5A4D_5A4D_5A4D;
        self.particles.clear();
        self.explosion_steps = 0;
        self.rebuilds = self.rebuilds.saturating_add(1);
        true
    }

    fn at(&self, x: usize, y: usize) -> u8 {
        self.grid[y * self.dot_cols + x]
    }

    fn put(&mut self, x: usize, y: usize, tier: u8) {
        self.grid[y * self.dot_cols + x] = tier;
    }

    /// Move a grain if the destination is free. Returns whether it moved.
    fn move_grain(&mut self, from: (usize, usize), to: (usize, usize)) -> bool {
        if self.at(to.0, to.1) != 0 {
            return false;
        }
        let tier = self.at(from.0, from.1);
        self.put(to.0, to.1, tier);
        self.put(from.0, from.1, 0);
        true
    }

    fn random(&mut self) -> f32 {
        rng_next(&mut self.rng)
    }
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.anim_frame());
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows < 4 || dot_cols < 4 || ctx.bands.is_empty() {
        return;
    }
    let steps = if state.ensure(dot_rows, dot_cols) {
        steps.max(1)
    } else {
        steps
    };

    let bass = band_avg(ctx.bands, 0, (ctx.bands.len() / 3).max(1));
    for _ in 0..steps {
        state.steps = state.steps.wrapping_add(1);
        if state.explosion_steps > 0 || !state.particles.is_empty() {
            advance_explosion(state);
            state.previous_bass = bass;
            continue;
        }
        emit(state, ctx.bands);

        let delta = bass - state.previous_bass;
        state.previous_bass = bass;
        let kick = delta > KICK_DELTA && bass > KICK_FLOOR;
        if kick && fill_fraction(state) > DETONATE_FILL {
            detonate(state);
            continue;
        }
        if kick {
            slap(state, delta, bass);
        }
        if bass > RUMBLE_FLOOR {
            rumble(state, bass);
        }
        fall(state);
        drain(state);
    }
}

/// Rain new grains from the top, one column region per band, coloured by
/// register: bass red, mids yellow, treble green.
fn emit(state: &mut State, bands: &[f32]) {
    let count = bands.len();
    let dot_cols = state.dot_cols;
    for (b, level) in bands.iter().enumerate() {
        // Short-circuit deliberately: a band under the floor must not draw
        // from the RNG, or the whole field's motion shifts with the spectrum.
        if *level < EMIT_FLOOR || state.random() > *level * EMIT_RATE {
            continue;
        }
        let centre = (b * 2 + 1) * dot_cols / (2 * count);
        let spread = (dot_cols / (count * 2)).max(1);
        let offset = (state.random() * (2 * spread) as f32) as usize;
        let x = (centre + offset).saturating_sub(spread).min(dot_cols - 1);
        let tier = if b < count / 3 {
            3
        } else if b < 2 * count / 3 {
            2
        } else {
            1
        };
        if state.at(x, 0) == 0 {
            state.put(x, 0, tier);
        }
    }
}

fn fill_fraction(state: &State) -> f32 {
    state.grid.iter().filter(|g| **g != 0).count() as f32 / state.grid.len() as f32
}

/// Speaker-cone slap: one violent lift across the whole bed on a kick's
/// rising edge, strongest nearest the bottom.
fn slap(state: &mut State, delta: f32, bass: f32) {
    let strength = (delta * 3.5 + bass * 0.8).min(1.4);
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    let deepest = (dot_rows - 1).max(1) as f32;
    // Top-down, so a grain lifted this step isn't visited again.
    for y in 0..dot_rows {
        let depth = y as f32 / deepest;
        let lift_probability = (strength * (0.30 + 0.70 * depth)).min(0.95);
        let lift_max = 2.0 + strength * 7.0 * (0.4 + 0.6 * depth);
        let spray = 1.0 + strength * 5.0;
        for x in 0..dot_cols {
            if state.at(x, y) == 0 || state.random() > lift_probability {
                continue;
            }
            let lift = 1 + (state.random() * lift_max) as usize;
            let jitter = (state.random() * (2.0 * spray + 1.0)) as i64 - spray as i64;
            let ny = y.saturating_sub(lift);
            let nx = (x as i64 + jitter).clamp(0, dot_cols as i64 - 1) as usize;
            state.move_grain((x, y), (nx, ny));
        }
    }
}

/// Sustained rumble: a small nudge every step while bass stays high, so a
/// held kick keeps the bed dancing instead of popping once and freezing.
/// Only the bottom half — that's what is coupled to the cone.
fn rumble(state: &mut State, bass: f32) {
    let strength = ((bass - RUMBLE_FLOOR) * 1.8).min(0.6);
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    let top = dot_rows / 2;
    let span = (dot_rows - 1 - top).max(1) as f32;
    for y in top..dot_rows {
        let probability = strength * (0.15 + 0.55 * (y - top) as f32 / span);
        for x in 0..dot_cols {
            if state.at(x, y) == 0 || state.random() > probability {
                continue;
            }
            let lift = 1 + (state.random() * 2.0) as usize;
            let jitter = (state.random() * 5.0) as i64 - 2;
            let ny = y.saturating_sub(lift);
            let nx = (x as i64 + jitter).clamp(0, dot_cols as i64 - 1) as usize;
            state.move_grain((x, y), (nx, ny));
        }
    }
}

/// Gravity pass, bottom-up so a grain moved into `y+1` isn't moved twice.
fn fall(state: &mut State) {
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    let left_first = state.steps.is_multiple_of(2);
    for y in (0..dot_rows - 1).rev() {
        for i in 0..dot_cols {
            let x = if left_first { i } else { dot_cols - 1 - i };
            if state.at(x, y) == 0 {
                continue;
            }
            if state.move_grain((x, y), (x, y + 1)) {
                continue;
            }
            // Blocked: slide onto the pile. Randomising which side is tried
            // first keeps slopes symmetric.
            let first = if state.random() < 0.5 { 1_i64 } else { -1 };
            for dx in [first, -first] {
                let nx = x as i64 + dx;
                if nx < 0 || nx >= dot_cols as i64 {
                    continue;
                }
                if state.move_grain((x, y), (nx as usize, y + 1)) {
                    break;
                }
            }
        }
    }
}

fn drain(state: &mut State) {
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    for x in 0..dot_cols {
        if state.at(x, dot_rows - 1) != 0 && state.random() < DRAIN_RATE {
            state.put(x, dot_rows - 1, 0);
        }
    }
}

/// Convert the whole bed into ballistic particles. Bottom grains carry more
/// upward energy, so the burst peaks from below.
fn detonate(state: &mut State) {
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    let deepest = (dot_rows - 1).max(1) as f32;
    state.particles.clear();
    for y in 0..dot_rows {
        let depth = y as f32 / deepest;
        for x in 0..dot_cols {
            let tier = state.at(x, y);
            if tier == 0 {
                continue;
            }
            state.put(x, y, 0);
            let vy = -(2.0 + state.random() * 5.0 + depth * 2.0);
            let vx = (state.random() - 0.5) * 8.0;
            state.particles.push(Particle {
                x: x as f32,
                y: y as f32,
                vx,
                vy,
                tier,
            });
        }
    }
    state.explosion_steps = EXPLODE_MAX_STEPS;
}

/// Advance the burst one step. The grid is rebuilt from the survivors, so
/// the renderer needs no special case.
fn advance_explosion(state: &mut State) {
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    state.grid.fill(0);

    let mut live = std::mem::take(&mut state.particles);
    live.retain_mut(|p| {
        p.vy += EXPLODE_GRAVITY;
        p.vx *= EXPLODE_DRAG;
        p.x += p.vx;
        p.y += p.vy;
        let (x, y) = (p.x as i64, p.y as i64);
        if x < 0 || y < 0 || x >= dot_cols as i64 || y >= dot_rows as i64 {
            return false;
        }
        state.grid[y as usize * dot_cols + x as usize] = p.tier;
        true
    });
    state.particles = live;

    state.explosion_steps = state.explosion_steps.saturating_sub(1);
    if state.particles.is_empty() {
        state.explosion_steps = 0;
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if state.dot_rows != dot_rows || state.dot_cols != dot_cols || state.grid.is_empty() {
        return;
    }
    let mut grid = BrailleGrid::new(dot_rows, dot_cols);
    for y in 0..dot_rows {
        for x in 0..dot_cols {
            let tier = state.at(x, y);
            if tier != 0 {
                grid.set(x, y, tier);
            }
        }
    }
    grid.render(area, buf, ctx.paint);
}
