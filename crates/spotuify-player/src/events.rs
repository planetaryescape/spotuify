//! Domain `PlayerEvent`s emitted by every backend.
//!
//! Smaller than librespot's internal event enum — only the events the
//! daemon needs to translate into wire-level `DaemonEvent`s. Backends
//! that don't have a particular signal simply don't emit it (Free-tier
//! `ConnectOnlyBackend` won't emit `PreloadNext`, for example).

use crate::DeviceId;

/// Player lifecycle and playback events. Sent through an
/// `UnboundedSender<PlayerEvent>` the backend captures at construction
/// time.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// Backend finished initialising and registered a Connect device.
    /// Translates to `DaemonEvent::PlayerReady`.
    Ready {
        device_id: DeviceId,
        name: String,
    },

    /// Transient backend hiccup (Spirc outer-timeout, audio sink
    /// recovery). Translates to `DaemonEvent::PlayerDegraded`.
    Degraded {
        reason: String,
    },

    /// Spotify account lacks Premium; backend cannot stream.
    /// Translates to `DaemonEvent::PremiumRequired`.
    PremiumRequired,

    /// librespot Session went invalid; backend will retry.
    /// Translates to `DaemonEvent::SessionDisconnected`.
    SessionDisconnected {
        reason: String,
    },

    /// Restart budget exhausted; backend is dead until manual recovery.
    /// Translates to `DaemonEvent::PlayerFailed`.
    Failed {
        reason: String,
        restarts: u32,
    },

    /// Playback began for a track. `position_ms` is the starting offset.
    PlaybackStarted {
        uri: String,
        position_ms: u32,
    },
    PlaybackPaused,
    PlaybackResumed,

    /// Currently-playing track changed (next/previous, queue advance).
    TrackChanged {
        uri: String,
        position_ms: u32,
    },

    /// Periodic position update while playing. Sent at the worker
    /// loop's tick cadence (~400ms). Daemon may aggregate.
    PositionTick {
        position_ms: u32,
    },

    /// Current track reached the end naturally.
    EndOfTrack {
        uri: String,
    },

    /// librespot's `TimeToPreloadNextTrack` signal. The URI is the
    /// current track reported by librespot; the daemon must look up the
    /// first upcoming queue item before calling `preload_uri`.
    /// Embedded backend only.
    PreloadNext {
        uri: String,
    },

    /// Device volume changed — emitted by the embedded backend on
    /// activation (librespot's initial volume) and after every honoured
    /// `set_volume`. `percent` is 0..=100. The daemon owns volume state,
    /// so this is how the embedded device's real volume reaches the
    /// snapshot and the devices list (the Web API reports it as `null`).
    VolumeChanged {
        percent: u8,
    },
}
