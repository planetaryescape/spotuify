//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_geyser.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, rng_next, BrailleGrid};
use super::{Ctx, StepClock};

const GRAVITY: f32 = 0.30;
const DRAG: f32 = 0.992;
/// Steps a droplet may live before it is dropped regardless of position. A
/// particle blown sideways can otherwise hover in-bounds indefinitely.
const MAX_LIFE: u16 = 200;
/// A bass rise this large, from at least this level, fires a burst.
const KICK_DELTA: f32 = 0.06;
const KICK_FLOOR: f32 = 0.15;
/// Droplets in a kick burst: this many plus a delta-scaled extra.
const BURST_BASE: u32 = 40;
const BURST_GAIN: f32 = 180.0;
/// How the registers weight the steady trickle. Bass dominates, so a bassline
/// alone keeps the column flowing.
const STEADY_BASS: f32 = 0.85;
const STEADY_MID: f32 = 0.25;
const STEADY_HIGH: f32 = 0.08;
/// Droplets per step at full steady loudness.
const STEADY_RATE: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
struct Droplet {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    tier: u8,
    life: u16,
}

/// A particle fountain rooted at the bottom centre. Loudness feeds a steady
/// mist; bass transients fire thick vertical jets. Every droplet then arcs
/// back down, and inherits the tier of the register that spawned it.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    grid: BrailleGrid,
    droplets: Vec<Droplet>,
    rng: u64,
    previous_bass: f32,
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
        if !self.grid.resize(dot_rows, dot_cols) {
            return false;
        }
        self.droplets.clear();
        self.rng = 0xFEED_5EED;
        self.rebuilds = self.rebuilds.saturating_add(1);
        true
    }

    fn random(&mut self) -> f32 {
        rng_next(&mut self.rng)
    }

    /// Launch one droplet from the jet, with jittered aim and speed and a
    /// tier sampled from the register mix.
    fn spawn(&mut self, jet_x: usize, floor: usize, spread: usize, speed: f32, mix: (f32, f32)) {
        let offset = (self.random() * (2 * spread + 1) as f32) as i64 - spread as i64;
        let vy = -speed * (0.6 + self.random() * 0.5);
        let vx = (self.random() - 0.5) * (1.0 + speed * 0.4);
        let roll = self.random();
        let (bass, mid) = mix;
        let tier = if roll < bass {
            3
        } else if roll < bass + mid {
            2
        } else {
            1
        };
        self.droplets.push(Droplet {
            x: (jet_x as i64 + offset) as f32,
            y: floor as f32,
            vx,
            vy,
            tier,
            life: 0,
        });
    }
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.frame);
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

    let count = ctx.bands.len();
    let bass = band_avg(ctx.bands, 0, (count / 3).max(1));
    let mid = band_avg(ctx.bands, count / 3, 2 * count / 3);
    let high = band_avg(ctx.bands, 2 * count / 3, count);

    let jet_x = dot_cols / 2;
    let spread = (dot_cols / 16).max(2);
    let steady = bass * STEADY_BASS + mid * STEADY_MID + high * STEADY_HIGH;

    for _ in 0..steps {
        let delta = bass - state.previous_bass;
        state.previous_bass = bass;

        for _ in 0..(steady * STEADY_RATE) as u32 {
            state.spawn(jet_x, dot_rows - 1, spread, 1.5 + steady * 4.5, (bass, mid));
        }
        if delta > KICK_DELTA && bass > KICK_FLOOR {
            let burst = BURST_BASE + (delta * BURST_GAIN) as u32;
            let speed = 4.5 + delta * 10.0 + bass * 4.0;
            for _ in 0..burst {
                state.spawn(jet_x, dot_rows - 1, spread * 2, speed, (bass, mid));
            }
        }

        advance(state, dot_rows, dot_cols);
    }
}

fn advance(state: &mut State, dot_rows: usize, dot_cols: usize) {
    state.grid.clear();
    let mut live = std::mem::take(&mut state.droplets);
    live.retain_mut(|d| {
        d.vy += GRAVITY;
        d.vx *= DRAG;
        d.x += d.vx;
        d.y += d.vy;
        d.life = d.life.saturating_add(1);
        let (x, y) = (d.x as i64, d.y as i64);
        if y >= dot_rows as i64 || x < 0 || x >= dot_cols as i64 || d.life > MAX_LIFE {
            return false;
        }
        // A droplet above the panel is still in flight and will fall back in;
        // clamp it to the top row rather than dropping it.
        state.grid.set(x as usize, y.max(0) as usize, d.tier);
        true
    });
    state.droplets = live;
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if !state.grid.matches(dot_rows, dot_cols) {
        return;
    }
    state.grid.render(area, buf, ctx.paint);
}
