//! Core domain types for spotuify.
//!
//! Per `docs/blueprint/01-architecture.md` §"Dependency rules", this crate has
//! **no internal dependencies**. Every other workspace member may import from
//! it; it imports from nothing in the workspace.
//!
//! These types describe the music domain — what plays, what's queued, what
//! devices exist, what playlists hold. IPC framing, HTTP semantics, storage
//! schema, and TUI rendering belong in other crates.

pub mod actions;
pub mod analytics;
pub mod ids;
mod lyrics_provider;
pub mod provider;
pub mod queue_merge;
pub mod uri;

pub use actions::{CommandKind, CommandResult, PlayContext};
pub use analytics::{
    action_finished_event, listen_qualified_event, now_ms, playback_completed_event,
    playback_paused_event, playback_resumed_event, playback_skipped_event, playback_started_event,
    provider_api_finished_event, qualify_listen, redact_provider_path, search_performed_event,
    AnalyticsEvent, AnalyticsEventKind, AnalyticsSink, AnalyticsSource, BackendLabel, HabitBucket,
    HabitWindow, ListenFact, MeasurementKind, PlaybackSource, Qualification, SkipReason,
    StoredAnalyticsEvent, QUALIFICATION_RULE_VERSION,
};
pub use ids::{AlbumId, ArtistId, PlaylistId, TrackId};
pub use lyrics_provider::{LyricsProvider, LyricsProviderParseError};
pub use provider::{
    AccessOutcome, AccessUnavailable, CatalogCaps, ClientPreferences, CollectionRequest,
    FreshnessProbe, LibraryCaps, LibraryRequest, MusicProvider, Mutation, MutationCompletion,
    MutationFailure, MutationOutcome, MutationReceipt, PageContinuation, PageRequest, PlayRequest,
    PlaySource, PlaylistCaps, PlaylistInsertion, PlaylistItemRef, ProviderCaps, ProviderCatalog,
    ProviderDescriptor, ProviderError, ProviderExtras, ProviderExtrasCaps, ProviderId,
    ProviderIdError, ProviderPage, ProviderResult, QueueAddRequest, RemoteTransport,
    RequestContext, RequestPriority, ResolvedTarget, SearchCaps, SearchRequest, TargetClaim,
    TransportCaps, TransportCommand, TransportDevice, TransportOutcome,
};
pub use uri::{ResourceUri, UriError, UriScheme, UriSchemeError};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Playback rate in hundredths (150 = 1.5×) so protocol types stay `Eq`
/// and a slider's `1.2500001` never becomes a distinct rate. Serialises
/// as a plain JSON number (`1.5`) for readability.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct PlaybackSpeed(u16);

impl PlaybackSpeed {
    pub const NORMAL: Self = Self(100);
    /// Spotify's podcast speed range.
    pub const MIN: Self = Self(50);
    pub const MAX: Self = Self(350);
    /// One notch of a speed picker.
    pub const STEP: u16 = 10;

    /// Clamp into the supported range, rounding to hundredths.
    pub fn from_f32(speed: f32) -> Self {
        let hundredths = (speed * 100.0).round();
        let clamped = hundredths.clamp(f32::from(Self::MIN.0), f32::from(Self::MAX.0));
        Self(clamped as u16)
    }

    pub fn as_f32(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    pub fn hundredths(self) -> u16 {
        self.0
    }

    pub fn is_normal(self) -> bool {
        self == Self::NORMAL
    }

    pub fn faster(self) -> Self {
        Self((self.0 + Self::STEP).min(Self::MAX.0))
    }

    pub fn slower(self) -> Self {
        Self(self.0.saturating_sub(Self::STEP).max(Self::MIN.0))
    }

    /// Parse `1.5`, `1.5x`, or `150%`.
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim().trim_end_matches(['x', 'X', '×']);
        let value = if let Some(percent) = trimmed.strip_suffix('%') {
            percent.trim().parse::<f32>().ok()? / 100.0
        } else {
            trimmed.parse::<f32>().ok()?
        };
        value.is_finite().then(|| Self::from_f32(value))
    }
}

impl Default for PlaybackSpeed {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl std::fmt::Display for PlaybackSpeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = self.0 / 100;
        let frac = self.0 % 100;
        if frac == 0 {
            write!(f, "{whole}x")
        } else if frac.is_multiple_of(10) {
            write!(f, "{whole}.{}x", frac / 10)
        } else {
            write!(f, "{whole}.{frac:02}x")
        }
    }
}

impl Serialize for PlaybackSpeed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // f64 from the integer hundredths prints `1.6`, not `1.600000023841858`.
        serializer.serialize_f64(f64::from(self.0) / 100.0)
    }
}

impl<'de> Deserialize<'de> for PlaybackSpeed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f32::deserialize(deserializer)?;
        if !value.is_finite() {
            return Err(D::Error::custom("playback speed must be finite"));
        }
        Ok(Self::from_f32(value))
    }
}

/// Number of bands in the parametric equalizer.
pub const EQ_BAND_COUNT: usize = 10;

/// Centre frequencies of the 10 EQ bands, in Hz.
pub const EQ_FREQUENCIES_HZ: [u32; EQ_BAND_COUNT] =
    [70, 180, 320, 600, 1000, 3000, 6000, 12000, 14000, 16000];

/// Q of every peaking filter. One value for all bands keeps the curve
/// predictable and matches the reference implementation.
pub const EQ_Q: f64 = 1.4;

/// Gain limits, in tenths of a decibel.
pub const EQ_MIN_TENTHS: i16 = -120;
pub const EQ_MAX_TENTHS: i16 = 120;

/// Named 10-band curves, in tenths of a dB per band.
///
/// Preset table from cliamp (MIT, (c) Bjarne Overli) —
/// <https://github.com/bjarneo/cliamp>, `ui/model/eq_presets.go`.
pub const EQ_PRESETS: [(&str, [i16; EQ_BAND_COUNT]); 16] = [
    ("Flat", [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    ("Rock", [50, 40, 20, -10, -20, 20, 40, 50, 50, 50]),
    ("Pop", [-10, 20, 40, 50, 40, 10, -10, -10, 10, 20]),
    ("Jazz", [30, 40, 20, 10, -10, -10, 10, 20, 30, 40]),
    ("Classical", [30, 20, 10, 0, -10, -10, 0, 20, 30, 40]),
    ("Bass Boost", [80, 60, 40, 20, 0, 0, 0, 0, 0, 0]),
    ("Treble Boost", [0, 0, 0, 0, 0, 10, 30, 50, 60, 70]),
    ("Vocal", [-20, -10, 10, 40, 50, 40, 20, 0, -10, -20]),
    ("Electronic", [60, 40, 10, -10, -20, 10, 30, 40, 50, 60]),
    ("Acoustic", [30, 30, 20, 0, 10, 20, 30, 30, 20, 10]),
    ("Hip-Hop", [70, 50, 30, 10, -10, -10, 10, 30, 30, 30]),
    ("R&B", [40, 60, 30, 10, -10, 10, 20, 20, 10, 0]),
    ("Loudness", [60, 40, 10, 0, -20, -10, 10, 40, 50, 50]),
    ("Late Night", [50, 30, 10, 0, -20, -10, 0, 20, 30, 30]),
    ("Podcast", [-30, -10, 20, 40, 40, 30, 10, -10, -20, -30]),
    ("Small Speakers", [70, 50, 40, 20, 10, 0, -10, 0, 10, 20]),
];

/// Sample rate the EQ response is evaluated at. librespot's rate is a
/// compile-time 44.1 kHz constant, so this is the only rate the filters
/// ever run at.
pub const EQ_SAMPLE_RATE_HZ: f64 = 44_100.0;

/// Attenuation applied before the filters so a boosted band cannot push a
/// full-scale sine past 1.0. Returns a negative dB value, or 0.0 when the
/// curve never exceeds unity (cut-only curves keep their level).
///
/// This is the *cascade* peak, not the largest single band: neighbouring
/// peaking filters overlap, so `Bass Boost` reaches +9.5 dB at 70 Hz even
/// though its tallest band is +8. Compensating per-band would still clip.
pub fn eq_headroom_db(bands_db: &[f64; EQ_BAND_COUNT]) -> f64 {
    let peak = eq_response_peak(bands_db).1;
    // A curve that never exceeds unity needs no headroom at all, margin
    // included: attenuating a flat EQ would be a bug you could hear.
    if peak <= 0.0 {
        0.0
    } else {
        -(peak + EQ_HEADROOM_MARGIN_DB)
    }
}

/// Slack added on top of the measured peak. The sweep below is refined to
/// well under a millidecibel, but float rounding through ten cascaded
/// biquads is not exactly the closed-form response the sweep evaluates, and
/// "cannot clip" should not rest on the last bit.
pub const EQ_HEADROOM_MARGIN_DB: f64 = 0.05;

/// Frequency, in Hz, where the cascade response is loudest. Tests and
/// diagnostics use it to probe a curve at its worst case instead of
/// guessing which band dominates.
pub fn eq_peak_frequency_hz(bands_db: &[f64; EQ_BAND_COUNT]) -> f64 {
    eq_response_peak(bands_db).0
}

/// Combined response of all ten bands at `freq_hz`, in dB.
pub fn eq_response_db(bands_db: &[f64; EQ_BAND_COUNT], freq_hz: f64) -> f64 {
    bands_db
        .iter()
        .enumerate()
        .map(|(index, gain)| {
            peaking_response_db(*gain, f64::from(EQ_FREQUENCIES_HZ[index]), freq_hz)
        })
        .sum()
}

/// `(frequency_hz, gain_db)` of the loudest point on the cascade.
///
/// A 256-point log sweep locates the peak's bracket, then a golden-section
/// search inside that bracket finds it properly. The coarse grid alone is
/// not enough: at 256 points its estimate of `Electronic` sits 0.014 dB
/// under the true peak, which is the difference between 0.999 and 1.0016 on
/// a full-scale sine.
fn eq_response_peak(bands_db: &[f64; EQ_BAND_COUNT]) -> (f64, f64) {
    const POINTS: usize = 256;
    let (low, high) = (20.0_f64.ln(), 20_000.0_f64.ln());
    let step = (high - low) / (POINTS - 1) as f64;
    let mut best = (low, f64::NEG_INFINITY);
    for point in 0..POINTS {
        let ln_freq = low + step * point as f64;
        let gain = eq_response_db(bands_db, ln_freq.exp());
        if gain > best.1 {
            best = (ln_freq, gain);
        }
    }
    // The true peak lies between the grid neighbours of the coarse argmax;
    // the response is smooth and single-peaked over one grid cell.
    refine_peak(
        bands_db,
        (best.0 - step).max(low),
        (best.0 + step).min(high),
    )
}

/// Golden-section maximisation over `[lo_ln, hi_ln]` in log-frequency.
/// 64 iterations shrink the bracket by ~1e-13, far below the margin.
fn refine_peak(bands_db: &[f64; EQ_BAND_COUNT], lo_ln: f64, hi_ln: f64) -> (f64, f64) {
    const INV_PHI: f64 = 0.618_033_988_749_894_9;
    let (mut low, mut high) = (lo_ln, hi_ln);
    let mut left = high - (high - low) * INV_PHI;
    let mut right = low + (high - low) * INV_PHI;
    let mut at_left = eq_response_db(bands_db, left.exp());
    let mut at_right = eq_response_db(bands_db, right.exp());
    for _ in 0..64 {
        if at_left > at_right {
            high = right;
            right = left;
            at_right = at_left;
            left = high - (high - low) * INV_PHI;
            at_left = eq_response_db(bands_db, left.exp());
        } else {
            low = left;
            left = right;
            at_left = at_right;
            right = low + (high - low) * INV_PHI;
            at_right = eq_response_db(bands_db, right.exp());
        }
    }
    let freq = ((low + high) / 2.0).exp();
    (freq, eq_response_db(bands_db, freq))
}

/// Magnitude response, in dB, of one peaking-EQ biquad at `freq`.
///
/// Audio EQ Cookbook coefficients (the same ones the player's `biquad`
/// filters use), evaluated on the unit circle:
/// `|H| = |b0 + b1·e^-jw + b2·e^-2jw| / |1 + a1·e^-jw + a2·e^-2jw|`.
fn peaking_response_db(gain_db: f64, centre_hz: f64, freq_hz: f64) -> f64 {
    if gain_db == 0.0 {
        return 0.0;
    }
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * centre_hz / EQ_SAMPLE_RATE_HZ;
    let alpha = w0.sin() / (2.0 * EQ_Q);
    let a0 = 1.0 + alpha / a;
    let (b0, b1, b2) = (
        (1.0 + alpha * a) / a0,
        (-2.0 * w0.cos()) / a0,
        (1.0 - alpha * a) / a0,
    );
    let (a1, a2) = ((-2.0 * w0.cos()) / a0, (1.0 - alpha / a) / a0);

    let w = std::f64::consts::TAU * freq_hz / EQ_SAMPLE_RATE_HZ;
    let magnitude_squared = |c0: f64, c1: f64, c2: f64| {
        let real = c0 + c1 * w.cos() + c2 * (2.0 * w).cos();
        let imaginary = -c1 * w.sin() - c2 * (2.0 * w).sin();
        real * real + imaginary * imaginary
    };
    let numerator = magnitude_squared(b0, b1, b2);
    let denominator = magnitude_squared(1.0, a1, a2);
    if denominator <= 0.0 {
        return 0.0;
    }
    10.0 * (numerator / denominator).log10()
}

/// Ten band gains, tenths of a dB internally so protocol types stay `Eq`
/// and `Hash`, plain dB numbers on the wire (`[5.0, 4.0, ...]`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EqBands([i16; EQ_BAND_COUNT]);

impl EqBands {
    /// Exactly `EQ_BAND_COUNT` finite gains, each within ±12 dB. Returns
    /// `None` otherwise: a partial curve is a caller bug, and silently
    /// clamping `100` to `12` would tell them they got what they asked for.
    pub fn from_db(bands: &[f32]) -> Option<Self> {
        if bands.len() != EQ_BAND_COUNT {
            return None;
        }
        let mut tenths = [0_i16; EQ_BAND_COUNT];
        for (slot, db) in tenths.iter_mut().zip(bands) {
            *slot = band_tenths_in_range(*db)?;
        }
        Some(Self(tenths))
    }

    /// Wrap gains that are already in tenths, e.g. a row of [`EQ_PRESETS`].
    pub fn from_tenths(tenths: [i16; EQ_BAND_COUNT]) -> Self {
        Self(tenths)
    }

    pub fn tenths(self) -> [i16; EQ_BAND_COUNT] {
        self.0
    }

    pub fn db(self) -> [f64; EQ_BAND_COUNT] {
        self.0.map(|tenths| f64::from(tenths) / 10.0)
    }
}

impl Serialize for EqBands {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // f32 built from integer tenths prints `5.0`, not `4.9999995`.
        self.0
            .map(|tenths| f32::from(tenths) / 10.0)
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EqBands {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bands = Vec::<f32>::deserialize(deserializer)?;
        Self::from_db(&bands).ok_or_else(|| {
            D::Error::custom(format!(
                "eq needs exactly {EQ_BAND_COUNT} finite band gains in dB, got {}",
                bands.len()
            ))
        })
    }
}

/// A 10-band parametric EQ curve plus the preset it came from.
///
/// Gains are tenths of a dB so the type stays `Eq`/`Hash` (protocol types
/// must be), and a slider's `4.9999998` never becomes a distinct curve.
/// On the wire the bands are plain dB numbers:
/// `{"preset":"Rock","bands":[5.0, 4.0, ...]}`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EqSettings {
    preset: Option<String>,
    bands: [i16; EQ_BAND_COUNT],
}

impl EqSettings {
    /// The zero curve, labelled with the `Flat` preset.
    pub fn flat() -> Self {
        Self {
            preset: Some(EQ_PRESETS[0].0.to_string()),
            bands: [0; EQ_BAND_COUNT],
        }
    }

    /// Look a preset up by case-insensitive name (`rock`, `Bass Boost`).
    pub fn from_preset(name: &str) -> Option<Self> {
        let trimmed = name.trim();
        EQ_PRESETS
            .iter()
            .find(|(preset, _)| preset.eq_ignore_ascii_case(trimmed))
            .map(|(preset, bands)| Self {
                preset: Some((*preset).to_string()),
                bands: *bands,
            })
    }

    /// A hand-edited ("Custom") curve.
    pub fn from_bands(bands: EqBands) -> Self {
        Self {
            preset: None,
            bands: bands.tenths(),
        }
    }

    /// Preset name, or `None` for a hand-edited ("Custom") curve.
    pub fn preset(&self) -> Option<&str> {
        self.preset.as_deref()
    }

    pub fn bands(&self) -> EqBands {
        EqBands(self.bands)
    }

    pub fn bands_tenths(&self) -> [i16; EQ_BAND_COUNT] {
        self.bands
    }

    pub fn bands_db(&self) -> [f64; EQ_BAND_COUNT] {
        self.bands.map(|tenths| f64::from(tenths) / 10.0)
    }

    /// Every band sits at 0 dB, i.e. the filters would be pure cost.
    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|tenths| *tenths == 0)
    }

    pub fn headroom_db(&self) -> f64 {
        eq_headroom_db(&self.bands_db())
    }

    /// Set one band (dB, clamped to ±12) and drop the preset label — the
    /// curve is no longer the preset the user picked.
    pub fn with_band(&self, index: usize, db: f32) -> Self {
        let mut bands = self.bands;
        if let Some(slot) = bands.get_mut(index) {
            *slot = clamp_band_tenths(db);
        }
        Self {
            preset: None,
            bands,
        }
    }

    /// Next preset in `EQ_PRESETS` order. A custom curve steps to `Flat`,
    /// so the cycle is Flat → … → Small Speakers → Flat, with Custom as a
    /// one-way exit.
    pub fn next_preset(&self) -> Self {
        let index = self
            .preset
            .as_deref()
            .and_then(|name| {
                EQ_PRESETS
                    .iter()
                    .position(|(preset, _)| preset.eq_ignore_ascii_case(name))
            })
            .unwrap_or(EQ_PRESETS.len() - 1);
        let (preset, bands) = EQ_PRESETS[(index + 1) % EQ_PRESETS.len()];
        Self {
            preset: Some(preset.to_string()),
            bands,
        }
    }

    /// Label for UI chips: the preset name, or `Custom` when hand-edited.
    pub fn label(&self) -> &str {
        self.preset.as_deref().unwrap_or("Custom")
    }
}

impl Default for EqSettings {
    fn default() -> Self {
        Self::flat()
    }
}

impl std::fmt::Display for EqSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Round to tenths, clamping into range. Used where a caller is stepping a
/// band rather than naming a value (the TUI's ±1 dB keys), so hitting the
/// rail is the intended behaviour rather than a rejected request.
fn clamp_band_tenths(db: f32) -> i16 {
    if !db.is_finite() {
        return 0;
    }
    let tenths = (db * 10.0).round();
    tenths.clamp(f32::from(EQ_MIN_TENTHS), f32::from(EQ_MAX_TENTHS)) as i16
}

/// Round to tenths, or `None` when the value is not a gain we support.
fn band_tenths_in_range(db: f32) -> Option<i16> {
    if !db.is_finite() {
        return None;
    }
    let tenths = (db * 10.0).round();
    (tenths >= f32::from(EQ_MIN_TENTHS) && tenths <= f32::from(EQ_MAX_TENTHS))
        .then_some(tenths as i16)
}

#[derive(Deserialize, Serialize)]
struct EqSettingsWire {
    #[serde(default)]
    preset: Option<String>,
    bands: EqBands,
}

impl Serialize for EqSettings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        EqSettingsWire {
            preset: self.preset.clone(),
            bands: self.bands(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EqSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = EqSettingsWire::deserialize(deserializer)?;
        Ok(Self {
            preset: wire.preset,
            bands: wire.bands.tenths(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Playback {
    pub item: Option<MediaItem>,
    pub device: Option<Device>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Phase 4 — when this snapshot was sampled by the daemon (Unix
    /// epoch ms). `None` on legacy payloads from older daemons. Clients
    /// can use it to compute staleness without trusting their own
    /// clock-skew with the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_at_ms: Option<i64>,
    /// Provider-reported state-transition time (Unix epoch ms), not when the
    /// response was sampled. The Spotify adapter maps its playback
    /// `timestamp` here. `None` outside remote-poll snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_timestamp_ms: Option<i64>,
    /// Provenance of this snapshot. Lets clients distinguish
    /// authoritative `PlayerEvent`/`CommandResult` state from
    /// best-effort `Cache`/`RecentFallback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlaybackStateSource>,
    /// Rate the current item is playing at. Podcast episodes follow the
    /// user's speed setting; music is always 1.0. `None` on snapshots from
    /// older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_speed: Option<PlaybackSpeed>,
}

/// Phase 4 — where a `Playback` snapshot came from. Highest-trust first.
/// Kebab-case wire format matches the other protocol enums.
///
/// Distinct from `analytics::PlaybackSource` (which records how the user
/// *got to* a track — playlist, queue, library, ...). This describes
/// *how the daemon learned* the current state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackStateSource {
    /// Local player event stream — sub-100ms after the audio actually
    /// changed state. Spotify adapter: librespot/spotifyd events.
    PlayerEvent,
    /// `CommandResult.playback` returned by `actions::execute` right
    /// after the mutation API call.
    CommandResult,
    /// Background provider playback-state poll. Eventually consistent.
    ///
    /// Serialization stays on the legacy `web-api-poll` label during the
    /// compatibility window so released clients continue to decode new
    /// daemon snapshots. New peers also accept the neutral `remote-poll`
    /// label for the eventual wire cutover.
    #[serde(rename = "web-api-poll", alias = "remote-poll")]
    RemotePoll,
    /// On-disk `playback_snapshots` row read at daemon startup or
    /// during cold-start, before any live signal landed.
    Cache,
    /// Synthesized "last played" from `recent_items` when no real
    /// playback snapshot exists. Always paused.
    RecentFallback,
}

/// Provider-policy reason emitted when the account tier forbids local
/// playback (e.g. Spotify free tier vs. the embedded librespot backend).
///
/// This exact string is load-bearing on the wire: the protocol's
/// `daemon_event_for_subscriber` downgrades a `ProviderPolicy` event carrying
/// it into the legacy `PremiumRequired` event for old clients. The player
/// produces it and the protocol matches on it, so it lives here in core where
/// both depend on it — if the two sides drift, the old-client downgrade path
/// breaks silently.
pub const PREMIUM_REQUIRED_POLICY_REASON: &str = "account tier does not permit local playback";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Queue {
    pub currently_playing: Option<MediaItem>,
    pub items: Vec<MediaItem>,
    /// True when the provider reported an active playback session at the
    /// time the snapshot was taken. False when the snapshot is being
    /// served from cache (the provider currently has no active session, so
    /// its queue endpoint returned empty and we are showing the last
    /// known items). Defaults to false for backward-compat with older
    /// peers that don't set the field — they get treated as cached.
    #[serde(default)]
    pub session_active: bool,
    /// Milliseconds since the epoch when the snapshot was captured.
    /// `0` means unknown (default-constructed). Matches the `i64`
    /// convention used by `Playback::sampled_at_ms` and the store's
    /// `fetched_at_ms` columns.
    #[serde(default)]
    pub as_of_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Track,
    Episode,
    Show,
    Album,
    Artist,
    Playlist,
}

/// Playback repeat behavior shared by protocol, provider, and player layers.
///
/// Serialization is canonical lowercase (`off`/`context`/`track`).
/// Deserialization is lenient on purpose: this type replaced a raw `String`
/// on the wire, and released daemons/clients (including the MCP bridge, which
/// forwards user input verbatim) exchange arbitrary values here. An unknown or
/// empty string decodes to [`RepeatMode::Off`] rather than failing the whole
/// frame — a strict unknown-variant error would kill the IPC connection with
/// no error response.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl<'de> Deserialize<'de> for RepeatMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or(Self::Off))
    }
}

impl RepeatMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RepeatModeParseError> {
        match value {
            "off" => Ok(Self::Off),
            "context" => Ok(Self::Context),
            "track" => Ok(Self::Track),
            other => Err(RepeatModeParseError {
                value: other.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for RepeatMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatModeParseError {
    pub value: String,
}

impl std::fmt::Display for RepeatModeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "repeat mode `{}` invalid (expected off, context, track)",
            self.value
        )
    }
}

impl std::error::Error for RepeatModeParseError {}

/// A provider-neutral release date with explicit precision.
///
/// The wire representation remains the legacy scalar string (`YYYY`,
/// `YYYY-MM`, or `YYYY-MM-DD`) so released clients keep decoding it. Provider
/// adapters parse their native date fields into this type before constructing
/// a [`MediaItem`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseDate {
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
}

impl ReleaseDate {
    pub fn new(
        year: u16,
        month: Option<u8>,
        day: Option<u8>,
    ) -> Result<Self, ReleaseDateParseError> {
        let value = match (month, day) {
            (None, None) => format!("{year:04}"),
            (Some(month), None) => format!("{year:04}-{month:02}"),
            (Some(month), Some(day)) => format!("{year:04}-{month:02}-{day:02}"),
            (None, Some(day)) => format!("{year:04}-??-{day:02}"),
        };
        validate_release_date(year, month, day)
            .map_err(|reason| ReleaseDateParseError { value, reason })?;
        Ok(Self { year, month, day })
    }

    /// Release year (always present).
    pub fn year(&self) -> u16 {
        self.year
    }

    /// Release month (1-12), when known.
    pub fn month(&self) -> Option<u8> {
        self.month
    }

    /// Release day (1-31), when known. Only present when [`Self::month`] is.
    pub fn day(&self) -> Option<u8> {
        self.day
    }
}

fn validate_release_date(
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
) -> Result<(), &'static str> {
    if year > 9999 {
        return Err("year must use at most four digits");
    }
    match (month, day) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err("day requires a month"),
        (Some(month), None) if (1..=12).contains(&month) => Ok(()),
        (Some(_), None) => Err("month must be between 01 and 12"),
        (Some(month), Some(day)) => {
            chrono::NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                .map(|_| ())
                .ok_or("date is not valid")
        }
    }
}

impl std::str::FromStr for ReleaseDate {
    type Err = ReleaseDateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts = value.split('-').collect::<Vec<_>>();
        let valid_widths = matches!(parts.as_slice(), [year] if year.len() == 4)
            || matches!(parts.as_slice(), [year, month] if year.len() == 4 && month.len() == 2)
            || matches!(parts.as_slice(), [year, month, day] if year.len() == 4 && month.len() == 2 && day.len() == 2);
        if !valid_widths {
            return Err(ReleaseDateParseError {
                value: value.to_string(),
                reason: "expected YYYY, YYYY-MM, or YYYY-MM-DD",
            });
        }
        let parse_error = || ReleaseDateParseError {
            value: value.to_string(),
            reason: "date components must be decimal numbers",
        };
        let year = parts[0].parse::<u16>().map_err(|_| parse_error())?;
        let month = parts
            .get(1)
            .map(|part| part.parse::<u8>().map_err(|_| parse_error()))
            .transpose()?;
        let day = parts
            .get(2)
            .map(|part| part.parse::<u8>().map_err(|_| parse_error()))
            .transpose()?;
        Self::new(year, month, day).map_err(|err| ReleaseDateParseError {
            value: value.to_string(),
            reason: err.reason,
        })
    }
}

impl std::fmt::Display for ReleaseDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.month, self.day) {
            (None, None) => write!(f, "{:04}", self.year),
            (Some(month), None) => write!(f, "{:04}-{month:02}", self.year),
            (Some(month), Some(day)) => write!(f, "{:04}-{month:02}-{day:02}", self.year),
            // `new()`/`FromStr` reject a day without a month, and the fields
            // are private, so this state is unconstructable. Degrade to
            // year-only precision rather than panic — Serialize routes through
            // Display, and a panic mid-encode would take down the daemon.
            (None, Some(_)) => write!(f, "{:04}", self.year),
        }
    }
}

impl Serialize for ReleaseDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReleaseDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseDateParseError {
    pub value: String,
    pub reason: &'static str,
}

impl std::fmt::Display for ReleaseDateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid release date `{}`: {}", self.value, self.reason)
    }
}

impl std::error::Error for ReleaseDateParseError {}

/// Tolerant field decoder for [`MediaItem::release_date`].
///
/// Old daemons serve raw cached date strings and providers have emitted junk
/// (e.g. Spotify's `"0000-00-00"`). A single malformed value must not fail the
/// whole containing payload, so an unparseable string decodes to `None` here.
/// The strict [`ReleaseDate`] `FromStr`/`Deserialize` impls stay available for
/// adapter-side parsing where a bad value should surface an error.
fn deserialize_lenient_release_date<'de, D>(
    deserializer: D,
) -> Result<Option<ReleaseDate>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| value.parse::<ReleaseDate>().ok()))
}

/// Provider-neutral album grouping used by discography clients.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AlbumGroup {
    Album,
    Single,
    Compilation,
    AppearsOn,
    Other(String),
}

impl AlbumGroup {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Album => "album",
            Self::Single => "single",
            Self::Compilation => "compilation",
            Self::AppearsOn => "appears_on",
            Self::Other(value) => value,
        }
    }
}

impl From<String> for AlbumGroup {
    fn from(value: String) -> Self {
        match value.as_str() {
            "album" => Self::Album,
            "single" => Self::Single,
            "compilation" => Self::Compilation,
            "appears_on" => Self::AppearsOn,
            _ => Self::Other(value),
        }
    }
}

impl From<&str> for AlbumGroup {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl std::fmt::Display for AlbumGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AlbumGroup {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AlbumGroup {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

/// Provenance of a media item. This records where metadata was obtained; it
/// is not the provider identity used for persistence keys.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ItemSource {
    Provider(String),
    Mercury,
    Local,
}

impl ItemSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Provider(provider) => provider,
            Self::Mercury => "mercury",
            Self::Local => "local",
        }
    }
}

impl From<String> for ItemSource {
    fn from(value: String) -> Self {
        match value.as_str() {
            "mercury" => Self::Mercury,
            "local" => Self::Local,
            _ => Self::Provider(value),
        }
    }
}

impl From<&str> for ItemSource {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl std::fmt::Display for ItemSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ItemSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ItemSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

/// A provider-neutral page of domain items.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
}

impl MediaKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Episode => "episode",
            Self::Show => "show",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for MediaKind {
    type Err = MediaKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "track" => Ok(Self::Track),
            "episode" => Ok(Self::Episode),
            "show" => Ok(Self::Show),
            "album" => Ok(Self::Album),
            "artist" => Ok(Self::Artist),
            "playlist" => Ok(Self::Playlist),
            other => Err(MediaKindParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaKindParseError {
    pub value: String,
}

impl std::fmt::Display for MediaKindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown media kind `{}`", self.value)
    }
}

impl std::error::Error for MediaKindParseError {}

/// A named reference to an artist, carrying the URI so clients can navigate
/// from a track/album straight to the artist without re-resolving by name.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArtistRef {
    pub name: String,
    pub uri: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct MediaItem {
    pub id: Option<String>,
    pub uri: String,
    pub name: String,
    pub subtitle: String,
    pub context: String,
    pub duration_ms: u64,
    pub image_url: Option<String>,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ItemSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_playable: Option<bool>,
    /// Album name for tracks (distinct from `context`, which the player rail
    /// reuses for the playback context label). `None` for non-track items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// When the item was saved/added (Unix epoch ms) — `added_at` from
    /// `/me/tracks` or a playlist's `added_at`. Enables "Date Added" sort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at_ms: Option<i64>,
    /// Episode resume position in milliseconds. Spotify adapter: maps from
    /// `resume_point.resume_position_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_position_ms: Option<u64>,
    /// Episode listened state. Spotify adapter: maps from
    /// `resume_point.fully_played`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fully_played: Option<bool>,
    /// Parsed release date for episodes/albums, preserving provider precision.
    /// Decodes leniently: a malformed cached/provider value becomes `None`
    /// rather than failing the whole payload (see
    /// [`deserialize_lenient_release_date`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_release_date"
    )]
    pub release_date: Option<ReleaseDate>,
    /// Album grouping relative to an artist. Spotify adapter: maps
    /// `album_group`, falling back to `album_type`. Unknown provider values
    /// remain available through [`AlbumGroup::Other`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_group: Option<AlbumGroup>,
    /// Whether this item is in the user's library (e.g. a saved album).
    /// Tagged by the daemon when listing an artist's discography so clients
    /// can offer an "in library only" filter without a refetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<bool>,
    /// Album URI for a track, so clients can navigate from a track to its
    /// album. `None` for non-track items or when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_uri: Option<String>,
    /// Contributing artists with their URIs, so clients can navigate from a
    /// track/album to each artist. Empty when unknown (older cached rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistRef>,
    /// Primary genre, when known. Provider adapters populate it best-effort;
    /// it flows live rather than being persisted, like `album_group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume_percent: Option<u8>,
    pub supports_volume: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub tracks_total: u64,
    pub image_url: Option<String>,
    /// Opaque provider version token used to skip unchanged playlist-track
    /// refetches. Missing tokens fail open and trigger a refetch.
    /// Rust uses the neutral name; the compatibility-stage wire key remains
    /// `snapshot_id` so released clients can decode new daemon responses.
    /// New peers also accept `version_token` input for the eventual cutover.
    #[serde(
        default,
        rename = "snapshot_id",
        alias = "version_token",
        skip_serializing_if = "Option::is_none"
    )]
    pub version_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricLine {
    pub start_ms: u64,
    pub text: String,
    pub is_rtl: bool,
}

pub fn active_lyric_line_index(
    lines: &[LyricLine],
    position_ms: u64,
    offset_ms: i64,
) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let adjusted = if offset_ms.is_negative() {
        position_ms.saturating_sub(offset_ms.unsigned_abs())
    } else {
        position_ms.saturating_add(offset_ms as u64)
    };
    let idx = lines.partition_point(|line| line.start_ms <= adjusted);
    Some(idx.saturating_sub(1))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncedLyrics {
    pub provider: LyricsProvider,
    pub track_uri: String,
    pub lines: Vec<LyricLine>,
    pub fetched_at_ms: i64,
    pub synced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

/// How often a reminder repeats. One-shot is `None`; the rest map to a
/// repeating calendar trigger and a next-occurrence computation in the
/// reminder's timezone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Recurrence {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
}

impl Recurrence {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" | "once" | "one-shot" => Some(Self::None),
            "daily" | "day" => Some(Self::Daily),
            "weekly" | "week" => Some(Self::Weekly),
            "monthly" | "month" => Some(Self::Monthly),
            _ => None,
        }
    }

    pub fn is_recurring(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Lifecycle of a reminder *schedule*.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderState {
    #[default]
    Active,
    Completed,
    Cancelled,
}

/// A scheduled reminder for a media item or grouping (track/album/playlist/
/// artist/show/episode). The daemon owns it; clients render/act. A media
/// snapshot is captured at creation so it still displays if the item changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Reminder {
    pub id: String,
    pub media_uri: String,
    pub media_kind: MediaKind,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// First/base due time (Unix epoch ms).
    pub anchor_at_ms: i64,
    pub recurrence: Recurrence,
    /// IANA timezone the anchor/recurrence is computed in.
    pub tz: String,
    /// Next time this reminder will fire (epoch ms). Advances on each fire.
    pub next_due_at_ms: i64,
    pub state: ReminderState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at_ms: i64,
}

/// Lifecycle of a fired reminder *occurrence* (an inbox notification).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationState {
    #[default]
    Unseen,
    Seen,
    Snoozed,
    Dismissed,
    Done,
}

/// A fired reminder occurrence shown in the notifications inbox. Media fields
/// are denormalized so the row survives the reminder being cancelled.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Notification {
    pub id: String,
    pub reminder_id: String,
    pub media_uri: String,
    pub media_kind: MediaKind,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// The occurrence's scheduled time (epoch ms).
    pub due_at_ms: i64,
    pub fired_at_ms: i64,
    pub state: NotificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until_ms: Option<i64>,
    /// "played" / "queued" once the user acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A saved position inside a media item (podcast episode, long mix, track)
/// with an optional note. Daemon-owned, stored locally; never sent to the
/// provider. The media snapshot is denormalized at creation so the row still
/// renders if the item later drops out of the cache.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Bookmark {
    pub id: String,
    pub media_uri: String,
    pub media_kind: MediaKind,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    pub position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at_ms: i64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn playback_speed_clamps_steps_parses_and_serialises_as_a_number() {
        assert_eq!(PlaybackSpeed::from_f32(0.1), PlaybackSpeed::MIN);
        assert_eq!(PlaybackSpeed::from_f32(9.0), PlaybackSpeed::MAX);
        assert_eq!(PlaybackSpeed::from_f32(1.2500001).hundredths(), 125);
        assert_eq!(PlaybackSpeed::NORMAL.faster().hundredths(), 110);
        assert_eq!(PlaybackSpeed::MIN.slower(), PlaybackSpeed::MIN);
        assert_eq!(PlaybackSpeed::MAX.faster(), PlaybackSpeed::MAX);
        assert_eq!(PlaybackSpeed::parse("1.5x").unwrap().hundredths(), 150);
        assert_eq!(PlaybackSpeed::parse("150%").unwrap().hundredths(), 150);
        assert_eq!(PlaybackSpeed::parse("2").unwrap().hundredths(), 200);
        assert!(PlaybackSpeed::parse("fast").is_none());
        assert_eq!(PlaybackSpeed::from_f32(1.5).to_string(), "1.5x");
        assert_eq!(PlaybackSpeed::from_f32(1.25).to_string(), "1.25x");
        assert_eq!(PlaybackSpeed::NORMAL.to_string(), "1x");
        assert_eq!(
            serde_json::to_string(&PlaybackSpeed::from_f32(1.5)).unwrap(),
            "1.5"
        );
        let decoded: PlaybackSpeed = serde_json::from_str("1.75").unwrap();
        assert_eq!(decoded.hundredths(), 175);
        assert!(serde_json::from_str::<PlaybackSpeed>("\"x\"").is_err());
    }

    #[test]
    fn eq_presets_are_named_uniquely_and_within_range() {
        assert_eq!(EQ_PRESETS.len(), 16);
        assert_eq!(EQ_PRESETS[0].0, "Flat");
        let mut names: Vec<String> = EQ_PRESETS
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        names.sort();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "preset names must be unique");
        for (name, bands) in EQ_PRESETS {
            for tenths in bands {
                assert!(
                    (EQ_MIN_TENTHS..=EQ_MAX_TENTHS).contains(&tenths),
                    "{name} band {tenths} out of range"
                );
            }
        }
        // Centre frequencies must be strictly ascending or the band index
        // the UI shows stops matching the frequency label.
        assert!(EQ_FREQUENCIES_HZ.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn eq_preset_lookup_is_case_insensitive_and_flat_is_default() {
        assert_eq!(EqSettings::default(), EqSettings::flat());
        assert!(EqSettings::flat().is_flat());
        assert_eq!(EqSettings::flat().preset(), Some("Flat"));
        let rock = EqSettings::from_preset("rOcK").unwrap();
        assert_eq!(rock.preset(), Some("Rock"));
        assert_eq!(rock.bands_db()[0], 5.0);
        assert!(!rock.is_flat());
        assert_eq!(
            EqSettings::from_preset("  bass boost ").unwrap().preset(),
            Some("Bass Boost")
        );
        assert!(EqSettings::from_preset("nope").is_none());
    }

    #[test]
    fn eq_bands_reject_gains_outside_the_supported_range() {
        // Clamping `100` to `12` would tell a caller they got what they
        // asked for. They did not.
        assert!(EqBands::from_db(&[0.0; EQ_BAND_COUNT]).is_some());
        assert!(EqBands::from_db(&[12.0; EQ_BAND_COUNT]).is_some());
        assert!(EqBands::from_db(&[-12.0; EQ_BAND_COUNT]).is_some());
        for bad in [12.05_f32, -12.05, 100.0, f32::NAN, f32::INFINITY] {
            let mut bands = [0.0_f32; EQ_BAND_COUNT];
            bands[3] = bad;
            assert!(
                EqBands::from_db(&bands).is_none(),
                "{bad} dB must be rejected, not clamped"
            );
        }
        // Wrong length stays a rejection too.
        assert!(EqBands::from_db(&[0.0, 1.0]).is_none());
        // Rounding to tenths still happens inside the range.
        assert_eq!(
            EqBands::from_db(&[1.44; EQ_BAND_COUNT]).unwrap().db()[0],
            1.4
        );

        // The wire inherits the rule through `EqSettings`.
        assert!(serde_json::from_str::<EqSettings>(
            r#"{"preset":null,"bands":[100,0,0,0,0,0,0,0,0,0]}"#
        )
        .is_err());
    }

    #[test]
    fn eq_band_edit_clears_the_preset_and_clamps() {
        let edited = EqSettings::from_preset("Rock").unwrap().with_band(4, -3.0);
        assert_eq!(edited.preset(), None);
        assert_eq!(edited.label(), "Custom");
        assert_eq!(edited.bands_db()[4], -3.0);
        // Other bands keep the preset's values.
        assert_eq!(edited.bands_db()[0], 5.0);
        assert_eq!(edited.with_band(0, 99.0).bands_db()[0], 12.0);
        assert_eq!(edited.with_band(0, -99.0).bands_db()[0], -12.0);
        // Out-of-range indices are a no-op, not a panic.
        assert_eq!(edited.with_band(10, 6.0).bands_db(), edited.bands_db());
        assert_eq!(edited.with_band(0, f32::NAN).bands_db()[0], 0.0);
    }

    #[test]
    fn eq_preset_cycle_wraps_and_custom_exits_to_flat() {
        let flat = EqSettings::flat();
        assert_eq!(flat.next_preset().preset(), Some("Rock"));
        let last = EqSettings::from_preset("Small Speakers").unwrap();
        assert_eq!(last.next_preset().preset(), Some("Flat"));
        let custom = flat.with_band(0, 6.0);
        assert_eq!(custom.next_preset().preset(), Some("Flat"));
    }

    #[test]
    fn eq_headroom_covers_the_whole_cascade_not_just_the_tallest_band() {
        assert_eq!(EqSettings::flat().headroom_db(), 0.0);
        // A cut-only curve never exceeds unity, so it keeps its level.
        assert_eq!(
            EqSettings::from_bands(EqBands::from_db(&[-6.0; 10]).unwrap()).headroom_db(),
            0.0
        );

        // Bass Boost's tallest band is +8 dB, but its neighbours pile on:
        // per-band compensation would still clip.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        assert!(
            bass.headroom_db() < -8.0,
            "headroom {} should exceed the tallest band",
            bass.headroom_db()
        );

        // Whatever the curve, applying the headroom must leave the peak
        // response at or below unity.
        for (name, bands) in EQ_PRESETS {
            let settings = EqSettings::from_preset(name).unwrap();
            let compensated = eq_headroom_db(&settings.bands_db()) + settings.headroom_db().abs();
            assert!(
                compensated <= 1e-9,
                "{name} still peaks {compensated} dB above unity ({bands:?})"
            );
        }
    }

    #[test]
    fn eq_headroom_survives_a_sweep_far_denser_than_the_one_it_uses() {
        // A 256-point grid can straddle a narrow peak: at that resolution
        // `Electronic` reads 0.014 dB low, which is 1.0016 on a full-scale
        // sine -- quiet clipping. The refined search plus the margin has to
        // hold against a sweep the implementation never performs.
        const DENSE: usize = 20_001;
        let (low, high) = (20.0_f64.ln(), 20_000.0_f64.ln());
        for (name, _) in EQ_PRESETS {
            let settings = EqSettings::from_preset(name).unwrap();
            let bands = settings.bands_db();
            let mut true_peak = f64::NEG_INFINITY;
            for point in 0..DENSE {
                let hz = (low + (high - low) * point as f64 / (DENSE - 1) as f64).exp();
                true_peak = true_peak.max(eq_response_db(&bands, hz));
            }
            let applied = -settings.headroom_db();
            if true_peak <= 0.0 {
                assert_eq!(applied, 0.0, "{name} only cuts; it needs no headroom");
                continue;
            }
            assert!(
                applied >= true_peak,
                "{name}: headroom {applied:.4} dB does not cover a true peak of {true_peak:.4} dB"
            );
            // ...and it must not be wildly over-generous either.
            assert!(
                applied - true_peak <= EQ_HEADROOM_MARGIN_DB + 0.01,
                "{name}: headroom {applied:.4} dB overshoots {true_peak:.4} dB"
            );
        }
    }

    #[test]
    fn eq_peak_frequency_is_the_argmax_of_the_cascade() {
        // Bass Boost's loudest point is BETWEEN its +8 (70 Hz) and +6
        // (180 Hz) bands, not at either centre — which is exactly why
        // probing band centres understates the headroom a curve needs.
        for (name, _) in EQ_PRESETS {
            let bands = EqSettings::from_preset(name).unwrap().bands_db();
            let peak_hz = eq_peak_frequency_hz(&bands);
            let peak_db = eq_response_db(&bands, peak_hz);
            for point in 0..256 {
                let hz = 20.0_f64 * (1_000.0_f64).powf(point as f64 / 255.0);
                assert!(
                    eq_response_db(&bands, hz) <= peak_db + 1e-9,
                    "{name}: {hz:.0} Hz is louder than the reported peak {peak_hz:.0} Hz"
                );
            }
            let expected = if peak_db <= 0.0 {
                0.0
            } else {
                -(peak_db + EQ_HEADROOM_MARGIN_DB)
            };
            assert_eq!(expected, eq_headroom_db(&bands));
        }
    }

    #[test]
    fn eq_band_response_peaks_at_its_own_centre_frequency() {
        // A single +12 dB band must deliver +12 dB at its centre and
        // essentially nothing a decade away.
        let at_centre = peaking_response_db(12.0, 1_000.0, 1_000.0);
        assert!((at_centre - 12.0).abs() < 0.05, "{at_centre}");
        let far = peaking_response_db(12.0, 1_000.0, 100.0);
        assert!(far.abs() < 0.5, "{far}");
        assert_eq!(peaking_response_db(0.0, 1_000.0, 1_000.0), 0.0);
    }

    #[test]
    fn eq_settings_round_trip_as_preset_plus_db_numbers() {
        let rock = EqSettings::from_preset("Rock").unwrap();
        let json = serde_json::to_string(&rock).unwrap();
        assert_eq!(
            json,
            r#"{"preset":"Rock","bands":[5.0,4.0,2.0,-1.0,-2.0,2.0,4.0,5.0,5.0,5.0]}"#
        );
        assert_eq!(serde_json::from_str::<EqSettings>(&json).unwrap(), rock);

        let custom = rock.with_band(0, 1.5);
        let json = serde_json::to_string(&custom).unwrap();
        assert!(
            json.starts_with(r#"{"preset":null,"bands":[1.5,"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<EqSettings>(&json).unwrap(), custom);

        // Wrong band count is a decode error, not a silently padded curve.
        assert!(serde_json::from_str::<EqSettings>(r#"{"bands":[0.0,0.0]}"#).is_err());
        assert!(serde_json::from_str::<EqSettings>("null").is_err());
    }

    #[test]
    fn media_kind_round_trips_through_json_lowercase() {
        let kinds = [
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Show,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ];
        for kind in kinds {
            let encoded = serde_json::to_string(&kind).expect("media kind should serialize");
            let decoded: MediaKind =
                serde_json::from_str(&encoded).expect("media kind should deserialize");
            assert_eq!(kind, decoded);
            assert_eq!(encoded.trim_matches('"'), kind.label());
            assert_eq!(kind.to_string(), kind.label());
            assert_eq!(
                kind.label().parse::<MediaKind>().expect("label parses"),
                kind
            );
        }
    }

    #[test]
    fn release_date_preserves_precision_and_legacy_scalar_wire_shape() {
        let year = "1999".parse::<ReleaseDate>().expect("year precision");
        let month = "1999-07".parse::<ReleaseDate>().expect("month precision");
        let day = "2000-02-29".parse::<ReleaseDate>().expect("leap day");

        assert_eq!((year.year, year.month, year.day), (1999, None, None));
        assert_eq!((month.year, month.month, month.day), (1999, Some(7), None));
        assert_eq!(day.to_string(), "2000-02-29");
        assert_eq!(serde_json::to_string(&day).unwrap(), "\"2000-02-29\"");
        assert_eq!(
            serde_json::from_str::<ReleaseDate>("\"1999-07\"").unwrap(),
            month
        );
        assert!("2001-02-29".parse::<ReleaseDate>().is_err());
        assert!("2024-13".parse::<ReleaseDate>().is_err());
        assert!("2024-1-01".parse::<ReleaseDate>().is_err());
    }

    #[test]
    fn media_item_release_date_field_decodes_leniently_to_none() {
        // A malformed date on one item must not fail the whole payload; the
        // field decodes to None while the good sibling keeps its value.
        let items: Vec<MediaItem> = serde_json::from_str(
            r#"[
                {"uri":"spotify:album:good","name":"Good","subtitle":"","context":"","duration_ms":0,"kind":"album","release_date":"1999-07-21"},
                {"uri":"spotify:album:junk","name":"Junk","subtitle":"","context":"","duration_ms":0,"kind":"album","release_date":"0000-00-00"}
            ]"#,
        )
        .expect("payload with one bad date should still decode");
        assert_eq!(
            items[0].release_date,
            Some("1999-07-21".parse::<ReleaseDate>().unwrap())
        );
        assert_eq!(items[1].release_date, None);
    }

    #[test]
    fn album_group_preserves_unknown_provider_values_on_scalar_wire() {
        let known = AlbumGroup::from("appears_on");
        let other = AlbumGroup::from("soundtrack");

        assert_eq!(known, AlbumGroup::AppearsOn);
        assert_eq!(other, AlbumGroup::Other("soundtrack".to_string()));
        assert_eq!(serde_json::to_string(&known).unwrap(), "\"appears_on\"");
        assert_eq!(
            serde_json::from_str::<AlbumGroup>("\"soundtrack\"").unwrap(),
            other
        );
    }

    #[test]
    fn item_source_is_typed_in_core_and_remains_a_scalar_on_wire() {
        let provider = ItemSource::from("provider-a");
        assert_eq!(provider, ItemSource::Provider("provider-a".to_string()));
        assert_eq!(ItemSource::from("mercury"), ItemSource::Mercury);
        assert_eq!(ItemSource::from("local"), ItemSource::Local);
        assert_eq!(serde_json::to_string(&provider).unwrap(), "\"provider-a\"");
        assert_eq!(
            serde_json::from_str::<ItemSource>("\"custom-provider\"").unwrap(),
            ItemSource::Provider("custom-provider".to_string())
        );
    }

    #[test]
    fn repeat_mode_round_trips_and_defaults_off() {
        for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
            assert_eq!(RepeatMode::parse(mode.label()).unwrap(), mode);
            let encoded = serde_json::to_string(&mode).unwrap();
            assert_eq!(serde_json::from_str::<RepeatMode>(&encoded).unwrap(), mode);
        }
        assert_eq!(RepeatMode::default(), RepeatMode::Off);
        assert!(RepeatMode::parse("loop").is_err());
    }

    #[test]
    fn repeat_mode_decodes_legacy_junk_to_off_without_failing_the_frame() {
        // The field replaced a raw `String`, and the MCP bridge forwards user
        // input verbatim, so decode must never error the containing frame.
        for junk in ["\"one\"", "\"on\"", "\"OFF\"", "\"\"", "\"track \""] {
            assert_eq!(
                serde_json::from_str::<RepeatMode>(junk).unwrap(),
                RepeatMode::Off,
                "junk value {junk} should decode to Off"
            );
        }
        // Canonical values still decode to their variant.
        assert_eq!(
            serde_json::from_str::<RepeatMode>("\"track\"").unwrap(),
            RepeatMode::Track
        );
        // Serialization stays canonical lowercase.
        assert_eq!(
            serde_json::to_string(&RepeatMode::Context).unwrap(),
            "\"context\""
        );
    }

    #[test]
    fn remote_poll_accepts_neutral_label_but_writes_legacy_wire_label() {
        assert_eq!(
            serde_json::to_string(&PlaybackStateSource::RemotePoll).unwrap(),
            "\"web-api-poll\""
        );
        assert_eq!(
            serde_json::from_str::<PlaybackStateSource>("\"web-api-poll\"").unwrap(),
            PlaybackStateSource::RemotePoll
        );
        assert_eq!(
            serde_json::from_str::<PlaybackStateSource>("\"remote-poll\"").unwrap(),
            PlaybackStateSource::RemotePoll
        );
    }

    #[test]
    fn playlist_uses_neutral_rust_field_and_legacy_wire_key() {
        let playlist = Playlist {
            id: "mix".to_string(),
            name: "Mix".to_string(),
            owner: "Owner".to_string(),
            tracks_total: 3,
            image_url: None,
            version_token: Some("version-1".to_string()),
        };
        let encoded = serde_json::to_value(&playlist).unwrap();
        assert_eq!(encoded["snapshot_id"], "version-1");
        assert!(encoded.get("version_token").is_none());

        let old_client_fixture = r#"{
            "id":"mix","name":"Mix","owner":"Owner","tracks_total":3,
            "image_url":null,"snapshot_id":"legacy-version"
        }"#;
        let old = serde_json::from_str::<Playlist>(old_client_fixture).unwrap();
        assert_eq!(old.version_token.as_deref(), Some("legacy-version"));

        let future_fixture = r#"{
            "id":"mix","name":"Mix","owner":"Owner","tracks_total":3,
            "image_url":null,"version_token":"neutral-version"
        }"#;
        let future = serde_json::from_str::<Playlist>(future_fixture).unwrap();
        assert_eq!(future.version_token.as_deref(), Some("neutral-version"));
    }

    #[test]
    fn generic_page_round_trips() {
        let page = Page {
            items: vec!["one".to_string(), "two".to_string()],
            total: 12,
            offset: 5,
        };
        let encoded = serde_json::to_string(&page).unwrap();
        assert_eq!(
            serde_json::from_str::<Page<String>>(&encoded).unwrap(),
            page
        );
    }

    #[test]
    fn lyrics_provider_round_trips_through_label_display_parse_and_json() {
        let providers = [LyricsProvider::Native, LyricsProvider::Lrclib];
        for provider in providers {
            let encoded =
                serde_json::to_string(&provider).expect("lyrics provider should serialize");
            let decoded: LyricsProvider =
                serde_json::from_str(&encoded).expect("lyrics provider should deserialize");
            assert_eq!(provider, decoded);
            assert_eq!(encoded.trim_matches('"'), provider.label());
            assert_eq!(provider.to_string(), provider.label());
            assert_eq!(
                provider
                    .label()
                    .parse::<LyricsProvider>()
                    .expect("label parses"),
                provider
            );
        }
    }

    #[test]
    fn media_item_omits_optional_fields_when_none() {
        let item = MediaItem {
            id: None,
            uri: "provider:track:abc".to_string(),
            name: "Song".to_string(),
            subtitle: String::new(),
            context: String::new(),
            duration_ms: 1000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        };
        let json = serde_json::to_value(&item).expect("media item should serialize");
        let obj = json.as_object().expect("media item JSON should be object");
        assert!(!obj.contains_key("source"));
        assert!(!obj.contains_key("freshness"));
        assert!(!obj.contains_key("explicit"));
        assert!(!obj.contains_key("is_playable"));
        assert!(!obj.contains_key("album"));
        assert!(!obj.contains_key("added_at_ms"));
        assert!(!obj.contains_key("resume_position_ms"));
        assert!(!obj.contains_key("fully_played"));
        assert!(!obj.contains_key("release_date"));
    }

    #[test]
    fn media_item_serializes_new_optional_fields_when_present() {
        let item = MediaItem {
            uri: "provider:track:abc".to_string(),
            name: "Song".to_string(),
            duration_ms: 1000,
            kind: MediaKind::Track,
            album: Some("Greatest Hits".to_string()),
            added_at_ms: Some(1_700_000_000_000),
            fully_played: Some(true),
            ..Default::default()
        };
        let json = serde_json::to_value(&item).expect("media item should serialize");
        assert_eq!(
            json.get("album").and_then(|v| v.as_str()),
            Some("Greatest Hits")
        );
        assert_eq!(
            json.get("added_at_ms").and_then(|v| v.as_i64()),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            json.get("fully_played").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn playback_default_is_paused_empty() {
        let p = Playback::default();
        assert!(p.item.is_none());
        assert!(p.device.is_none());
        assert!(!p.is_playing);
        assert_eq!(p.progress_ms, 0);
    }

    #[test]
    fn device_renames_kind_to_type_in_json() {
        let device = Device {
            id: Some("dev1".to_string()),
            name: "Phone".to_string(),
            kind: "smartphone".to_string(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        };
        let json = serde_json::to_value(&device).expect("device should serialize");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("smartphone")
        );
        assert!(json.get("kind").is_none());
    }

    #[test]
    fn active_lyric_line_index_uses_offset_adjusted_position() {
        let lines = vec![lyric_line(1_000), lyric_line(2_000), lyric_line(5_000)];

        assert_eq!(active_lyric_line_index(&lines, 2_500, 0), Some(1));
        assert_eq!(active_lyric_line_index(&lines, 1_500, 700), Some(1));
        assert_eq!(active_lyric_line_index(&lines, 2_500, -700), Some(0));
    }

    fn lyric_line(start_ms: u64) -> LyricLine {
        LyricLine {
            start_ms,
            text: start_ms.to_string(),
            is_rtl: false,
        }
    }
}

#[cfg(test)]
mod dev_dependencies_imports {
    // Required because serde_json is a dev-dependency of this crate but not a
    // direct dependency. The test module uses it via `serde_json::*` paths.
    #[allow(unused_imports)]
    use serde_json as _;
}
