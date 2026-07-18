//! Domain `PlayerEvent`s emitted by every backend.
//!
//! Only signals the daemon can consume are represented. Backends that do not
//! have a particular signal simply do not emit it.

use crate::{DeviceId, ResourceUri};

/// Player lifecycle and playback events. Sent through an
/// `UnboundedSender<PlayerEvent>` the backend captures at construction
/// time.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PlayerEvent {
    /// Backend finished initialising and registered a playback device.
    /// Translates to `DaemonEvent::PlayerReady`.
    Ready {
        device_id: DeviceId,
        name: String,
    },

    /// Transient backend hiccup or audio-sink recovery.
    Degraded {
        reason: String,
    },

    /// Provider policy prevents local playback (for example, an account tier
    /// or regional restriction).
    ProviderPolicy {
        reason: String,
    },

    /// The provider playback session went invalid; backend will retry.
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
        uri: ResourceUri,
        position_ms: u32,
    },
    PlaybackPaused,
    PlaybackResumed,

    /// Currently-playing track changed (next/previous, queue advance).
    TrackChanged {
        uri: ResourceUri,
        position_ms: u32,
    },

    /// Periodic position update while playing. Sent at the worker
    /// loop's tick cadence (~400ms). Daemon may aggregate.
    PositionTick {
        position_ms: u32,
    },

    /// Current track reached the end naturally.
    EndOfTrack {
        uri: ResourceUri,
    },

    /// The URI is the current item reported by the backend; the daemon looks
    /// up the first upcoming queue item before calling `preload_uri`.
    PreloadNext {
        uri: ResourceUri,
    },

    /// Device volume changed — emitted by the embedded backend on
    /// activation and after every honoured `set_volume`. `percent` is
    /// 0..=100. The daemon owns volume state, so this is how the local
    /// device's real volume reaches snapshots and device lists.
    VolumeChanged {
        percent: u8,
    },
}
