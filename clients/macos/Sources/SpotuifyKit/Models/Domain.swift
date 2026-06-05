import Foundation

// Core domain types mirroring `spotuify_core`. Field names map the daemon's
// snake_case JSON via explicit CodingKeys (the daemon uses no rename_all on
// these structs, so keys are verbatim). All types are immutable value types
// and Sendable so they can cross from the IO actor to @MainActor stores.

public enum MediaKind: String, Codable, Sendable, Hashable {
    case track, episode, show, album, artist, playlist
    case other

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = MediaKind(rawValue: raw) ?? .other
    }
}

public struct MediaItem: Codable, Sendable, Hashable, Identifiable {
    public let spotifyID: String?
    public let uri: String
    public let name: String
    public let subtitle: String
    public let context: String
    public let durationMs: UInt64
    public let imageURL: String?
    public let kind: MediaKind
    public let source: String?
    public let freshness: String?
    public let explicit: Bool?
    public let isPlayable: Bool?
    public let album: String?
    public let addedAtMs: Int64?
    public let resumePositionMs: UInt64?
    public let fullyPlayed: Bool?
    public let releaseDate: String?

    public init(
        spotifyID: String? = nil,
        uri: String,
        name: String,
        subtitle: String = "",
        context: String = "",
        durationMs: UInt64 = 0,
        imageURL: String? = nil,
        kind: MediaKind = .track,
        source: String? = nil,
        freshness: String? = nil,
        explicit: Bool? = nil,
        isPlayable: Bool? = nil,
        album: String? = nil,
        addedAtMs: Int64? = nil,
        resumePositionMs: UInt64? = nil,
        fullyPlayed: Bool? = nil,
        releaseDate: String? = nil
    ) {
        self.spotifyID = spotifyID
        self.uri = uri
        self.name = name
        self.subtitle = subtitle
        self.context = context
        self.durationMs = durationMs
        self.imageURL = imageURL
        self.kind = kind
        self.source = source
        self.freshness = freshness
        self.explicit = explicit
        self.isPlayable = isPlayable
        self.album = album
        self.addedAtMs = addedAtMs
        self.resumePositionMs = resumePositionMs
        self.fullyPlayed = fullyPlayed
        self.releaseDate = releaseDate
    }

    /// Stable identity for SwiftUI. The Spotify `id` is optional and not
    /// always unique across kinds, but `uri` is the canonical handle.
    public var id: String { uri }

    /// Best album label for display: the dedicated field, falling back to
    /// `context` (which the daemon fills with the album for tracks).
    public var albumLabel: String? {
        if let album, !album.isEmpty { return album }
        return context.isEmpty ? nil : context
    }

    /// Episode listened state.
    public var isFullyPlayed: Bool { fullyPlayed == true }
    public var isInProgress: Bool { (resumePositionMs ?? 0) > 0 && !isFullyPlayed }

    enum CodingKeys: String, CodingKey {
        case spotifyID = "id"
        case uri, name, subtitle, context
        case durationMs = "duration_ms"
        case imageURL = "image_url"
        case kind, source, freshness, explicit
        case isPlayable = "is_playable"
        case album
        case addedAtMs = "added_at_ms"
        case resumePositionMs = "resume_position_ms"
        case fullyPlayed = "fully_played"
        case releaseDate = "release_date"
    }
}

public struct Device: Codable, Sendable, Hashable, Identifiable {
    public let deviceID: String?
    public let name: String
    public let kind: String
    public let isActive: Bool
    public let isRestricted: Bool
    public let volumePercent: UInt8?
    public let supportsVolume: Bool

    public var id: String { deviceID ?? name }

    enum CodingKeys: String, CodingKey {
        case deviceID = "id"
        case name
        case kind = "type"
        case isActive = "is_active"
        case isRestricted = "is_restricted"
        case volumePercent = "volume_percent"
        case supportsVolume = "supports_volume"
    }
}

public struct Playback: Codable, Sendable, Equatable {
    public let item: MediaItem?
    public let device: Device?
    public let isPlaying: Bool
    public let progressMs: UInt64
    public let shuffle: Bool
    public let repeatMode: String
    public let sampledAtMs: Int64?
    public let providerTimestampMs: Int64?
    public let source: String?

    enum CodingKeys: String, CodingKey {
        case item, device
        case isPlaying = "is_playing"
        case progressMs = "progress_ms"
        case shuffle
        case repeatMode = "repeat"
        case sampledAtMs = "sampled_at_ms"
        case providerTimestampMs = "provider_timestamp_ms"
        case source
    }
}

public struct Queue: Codable, Sendable, Equatable {
    public let currentlyPlaying: MediaItem?
    public let items: [MediaItem]
    /// `session_active` / `as_of_ms` carry `#[serde(default)]` on the daemon,
    /// so they may be absent on older snapshots — modelled as optionals.
    public let sessionActive: Bool?
    public let asOfMs: Int64?

    public var isSessionActive: Bool { sessionActive ?? false }

    enum CodingKeys: String, CodingKey {
        case currentlyPlaying = "currently_playing"
        case items
        case sessionActive = "session_active"
        case asOfMs = "as_of_ms"
    }
}

public struct Playlist: Codable, Sendable, Hashable, Identifiable {
    public let id: String
    public let name: String
    public let owner: String
    public let tracksTotal: UInt64
    public let imageURL: String?
    public let snapshotID: String?

    enum CodingKeys: String, CodingKey {
        case id, name, owner
        case tracksTotal = "tracks_total"
        case imageURL = "image_url"
        case snapshotID = "snapshot_id"
    }
}

public struct LyricLine: Codable, Sendable, Hashable {
    public let startMs: UInt64
    public let text: String
    public let isRtl: Bool

    enum CodingKeys: String, CodingKey {
        case startMs = "start_ms"
        case text
        case isRtl = "is_rtl"
    }
}

public struct SyncedLyrics: Codable, Sendable, Equatable {
    public let provider: String
    public let trackURI: String
    public let lines: [LyricLine]
    public let fetchedAtMs: Int64
    public let synced: Bool
    public let language: String?
    public let sourceURL: String?

    enum CodingKeys: String, CodingKey {
        case provider
        case trackURI = "track_uri"
        case lines
        case fetchedAtMs = "fetched_at_ms"
        case synced, language
        case sourceURL = "source_url"
    }

    /// Index of the line active at `positionMs` (with a per-track `offsetMs`
    /// tweak), mirroring `spotuify_core::active_lyric_line_index`.
    public func activeLineIndex(positionMs: UInt64, offsetMs: Int64) -> Int? {
        guard !lines.isEmpty else { return nil }
        let adjusted: UInt64
        if offsetMs < 0 {
            adjusted = positionMs >= UInt64(-offsetMs) ? positionMs - UInt64(-offsetMs) : 0
        } else {
            adjusted = positionMs &+ UInt64(offsetMs)
        }
        let count = lines.prefix { $0.startMs <= adjusted }.count
        return count == 0 ? nil : count - 1
    }
}

/// Read-only startup snapshot returned by `client-seed`.
public struct ClientSeed: Decodable, Sendable {
    public let playback: Playback
    public let queue: Queue
    public let devices: [Device]
    public let recent: [MediaItem]
}
