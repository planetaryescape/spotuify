//! Provider-neutral command models shared by daemon-backed clients.

use crate::{Device, MediaItem, Playback, Queue, RepeatMode};

#[derive(Clone, Debug, Default)]
pub struct PlayContext {
    pub context_uri: Option<String>,
    pub tracks: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub enum CommandKind {
    Pause,
    Resume,
    TogglePlayback,
    PlayItem {
        item: MediaItem,
    },
    PlayUri {
        uri: String,
        context: Option<PlayContext>,
    },
    Next,
    Previous,
    Seek {
        position_ms: u64,
    },
    Volume {
        volume_percent: u8,
    },
    Shuffle {
        state: bool,
    },
    Repeat {
        state: RepeatMode,
    },
    QueueItem {
        item: MediaItem,
    },
    QueueUri {
        uri: String,
    },
    Transfer {
        device: Device,
        play: bool,
    },
    AddToPlaylist {
        item: MediaItem,
        playlist_id: String,
        playlist_name: String,
    },
    SaveItem {
        item: MediaItem,
    },
    SaveCurrent,
}

#[derive(Clone, Debug, Default)]
pub struct CommandResult {
    pub message: Option<String>,
    pub playback: Option<Playback>,
    pub queue: Option<Queue>,
    pub devices: Option<Vec<Device>>,
    pub request_refresh: bool,
}
