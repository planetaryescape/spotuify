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

/// Stable provider registry identity. Its single-string wire representation
/// mirrors `spotuify_core::ProviderId` and rejects malformed identifiers.
public struct ProviderID: RawRepresentable, Codable, Sendable, Hashable, CustomStringConvertible {
    public let rawValue: String

    public init?(rawValue: String) {
        let bytes = Array(rawValue.utf8)
        guard let first = bytes.first,
              first >= 97, first <= 122,
              bytes.dropFirst().allSatisfy({ byte in
                  (byte >= 97 && byte <= 122)
                      || (byte >= 48 && byte <= 57)
                      || byte == 45
              })
        else { return nil }
        self.rawValue = rawValue
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let value = try container.decode(String.self)
        guard let provider = ProviderID(rawValue: value) else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Invalid provider id \(value)")
        }
        self = provider
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    public var description: String { rawValue }

    /// Compatibility identity used by the released scalar search-source wire.
    public static let spotify = ProviderID(unchecked: "spotify")

    private init(unchecked value: String) {
        rawValue = value
    }
}

/// A named reference to an artist carrying its URI, so a track/album row can
/// navigate straight to the artist. Mirrors `spotuify_core::ArtistRef`.
public struct ArtistRef: Codable, Sendable, Hashable, Identifiable {
    public let name: String
    public let uri: String

    public init(name: String, uri: String) {
        self.name = name
        self.uri = uri
    }

    public var id: String { uri }
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
    /// Spotify's per-artist `album_group` (album/single/compilation/appears_on);
    /// drives the discography sections. `nil` for non-album items.
    public let albumGroup: String?
    /// Whether this album is already in the user's library (tagged by the
    /// daemon for an artist's discography). `nil` when not applicable.
    public let inLibrary: Bool?
    /// Album URI for a track, enabling navigation to the album. `nil` when
    /// unknown (older cached rows / non-track items).
    public let albumURI: String?
    /// Contributing artists with URIs, enabling navigation to each artist.
    /// Empty when unknown.
    public let artists: [ArtistRef]
    /// Primary genre, when known (Spotify carries it on the artist/album, so
    /// it's populated best-effort). `nil` when unknown.
    public let genre: String?

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
        releaseDate: String? = nil,
        albumGroup: String? = nil,
        inLibrary: Bool? = nil,
        albumURI: String? = nil,
        artists: [ArtistRef] = [],
        genre: String? = nil
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
        self.albumGroup = albumGroup
        self.inLibrary = inLibrary
        self.albumURI = albumURI
        self.artists = artists
        self.genre = genre
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

    /// A secondary metadata line for collection rows/tiles (distinct from the
    /// artist/owner `subtitle`): year + track count for albums, follower count
    /// for artists, episode count for shows, track count for playlists. `nil`
    /// when there's nothing extra to show.
    public var metaLine: String? {
        var parts: [String] = []
        if kind == .album, let releaseDate, releaseDate.count >= 4 {
            parts.append(String(releaseDate.prefix(4)))
        }
        if !context.isEmpty { parts.append(context) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// Synthetic artist items (kind `.artist`) for click-through navigation
    /// from a track/album row. Only artists carrying a URI are navigable.
    public var artistNavItems: [MediaItem] {
        artists.filter { !$0.uri.isEmpty }.map {
            MediaItem(uri: $0.uri, name: $0.name, kind: .artist)
        }
    }

    /// Synthetic album item (kind `.album`) for navigating from a track to its
    /// album. `nil` when the album URI is unknown.
    public var albumNavItem: MediaItem? {
        guard let albumURI, !albumURI.isEmpty else { return nil }
        return MediaItem(
            uri: albumURI, name: albumLabel ?? "Album", imageURL: imageURL, kind: .album)
    }

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
        case albumGroup = "album_group"
        case inLibrary = "in_library"
        case albumURI = "album_uri"
        case artists
        case genre
    }

    // Custom decoder so the daemon's `skip_serializing_if`'d fields (notably
    // `artists`, omitted when empty) decode to sensible defaults instead of
    // failing. Encoding stays synthesized.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        spotifyID = try c.decodeIfPresent(String.self, forKey: .spotifyID)
        uri = try c.decode(String.self, forKey: .uri)
        name = try c.decode(String.self, forKey: .name)
        subtitle = try c.decodeIfPresent(String.self, forKey: .subtitle) ?? ""
        context = try c.decodeIfPresent(String.self, forKey: .context) ?? ""
        durationMs = try c.decodeIfPresent(UInt64.self, forKey: .durationMs) ?? 0
        imageURL = try c.decodeIfPresent(String.self, forKey: .imageURL)
        kind = try c.decodeIfPresent(MediaKind.self, forKey: .kind) ?? .track
        source = try c.decodeIfPresent(String.self, forKey: .source)
        freshness = try c.decodeIfPresent(String.self, forKey: .freshness)
        explicit = try c.decodeIfPresent(Bool.self, forKey: .explicit)
        isPlayable = try c.decodeIfPresent(Bool.self, forKey: .isPlayable)
        album = try c.decodeIfPresent(String.self, forKey: .album)
        addedAtMs = try c.decodeIfPresent(Int64.self, forKey: .addedAtMs)
        resumePositionMs = try c.decodeIfPresent(UInt64.self, forKey: .resumePositionMs)
        fullyPlayed = try c.decodeIfPresent(Bool.self, forKey: .fullyPlayed)
        releaseDate = try c.decodeIfPresent(String.self, forKey: .releaseDate)
        albumGroup = try c.decodeIfPresent(String.self, forKey: .albumGroup)
        inLibrary = try c.decodeIfPresent(Bool.self, forKey: .inLibrary)
        albumURI = try c.decodeIfPresent(String.self, forKey: .albumURI)
        artists = try c.decodeIfPresent([ArtistRef].self, forKey: .artists) ?? []
        genre = try c.decodeIfPresent(String.self, forKey: .genre)
    }
}

/// One listening session — a run of consecutively-played tracks. Mirrors
/// `spotuify_protocol::ListenSession`.
public struct ListenSession: Codable, Sendable, Hashable, Identifiable {
    public let sessionID: String
    public let startedAtMs: Int64
    public let endedAtMs: Int64
    public let trackCount: UInt32
    public let contextLabel: String?
    public let tracks: [MediaItem]

    public var id: String { sessionID }

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case startedAtMs = "started_at_ms"
        case endedAtMs = "ended_at_ms"
        case trackCount = "track_count"
        case contextLabel = "context_label"
        case tracks
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

// MARK: - Provider capabilities

public struct ProviderSearchCapabilities: Codable, Sendable, Equatable {
    public let remote: Bool
    public let kinds: [MediaKind]
    public let maxPageSize: Int?
    public let maxQueryCharacters: Int?

    enum CodingKeys: String, CodingKey {
        case remote, kinds
        case maxPageSize = "max_page_size"
        case maxQueryCharacters = "max_query_chars"
    }

    public init() {
        remote = false
        kinds = []
        maxPageSize = nil
        maxQueryCharacters = nil
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        remote = try container.decodeIfPresent(Bool.self, forKey: .remote) ?? false
        kinds = try container.decodeIfPresent([MediaKind].self, forKey: .kinds) ?? []
        maxPageSize = try container.decodeIfPresent(Int.self, forKey: .maxPageSize)
        maxQueryCharacters = try container.decodeIfPresent(Int.self, forKey: .maxQueryCharacters)
    }

    static let empty = ProviderSearchCapabilities()
}

public struct ProviderCatalogCapabilities: Codable, Sendable, Equatable {
    public let lookupKinds: [MediaKind]
    public let recentlyPlayed: Bool
    public let recentlyPlayedMaxPageSize: Int?
    public let albumTracks: Bool
    public let albumTracksMaxPageSize: Int?
    public let artistAlbums: Bool
    public let artistAlbumsMaxPageSize: Int?
    public let showEpisodes: Bool
    public let showEpisodesMaxPageSize: Int?

    enum CodingKeys: String, CodingKey {
        case lookupKinds = "lookup_kinds"
        case recentlyPlayed = "recently_played"
        case recentlyPlayedMaxPageSize = "recently_played_max_page_size"
        case albumTracks = "album_tracks"
        case albumTracksMaxPageSize = "album_tracks_max_page_size"
        case artistAlbums = "artist_albums"
        case artistAlbumsMaxPageSize = "artist_albums_max_page_size"
        case showEpisodes = "show_episodes"
        case showEpisodesMaxPageSize = "show_episodes_max_page_size"
    }

    public init() {
        lookupKinds = []
        recentlyPlayed = false
        recentlyPlayedMaxPageSize = nil
        albumTracks = false
        albumTracksMaxPageSize = nil
        artistAlbums = false
        artistAlbumsMaxPageSize = nil
        showEpisodes = false
        showEpisodesMaxPageSize = nil
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        lookupKinds = try container.decodeIfPresent([MediaKind].self, forKey: .lookupKinds) ?? []
        recentlyPlayed = try container.decodeIfPresent(Bool.self, forKey: .recentlyPlayed) ?? false
        recentlyPlayedMaxPageSize = try container.decodeIfPresent(
            Int.self, forKey: .recentlyPlayedMaxPageSize)
        albumTracks = try container.decodeIfPresent(Bool.self, forKey: .albumTracks) ?? false
        albumTracksMaxPageSize = try container.decodeIfPresent(
            Int.self, forKey: .albumTracksMaxPageSize)
        artistAlbums = try container.decodeIfPresent(Bool.self, forKey: .artistAlbums) ?? false
        artistAlbumsMaxPageSize = try container.decodeIfPresent(
            Int.self, forKey: .artistAlbumsMaxPageSize)
        showEpisodes = try container.decodeIfPresent(Bool.self, forKey: .showEpisodes) ?? false
        showEpisodesMaxPageSize = try container.decodeIfPresent(
            Int.self, forKey: .showEpisodesMaxPageSize)
    }

    static let empty = ProviderCatalogCapabilities()
}

public struct ProviderLibraryCapabilities: Codable, Sendable, Equatable {
    public let readKinds: [MediaKind]
    public let saveKinds: [MediaKind]
    public let followKinds: [MediaKind]
    public let mutationMaxBatch: Int?
    public let maxPageSize: Int?
    public let freshnessProbe: Bool

    enum CodingKeys: String, CodingKey {
        case readKinds = "read_kinds"
        case saveKinds = "save_kinds"
        case writeKinds = "write_kinds"
        case followKinds = "follow_kinds"
        case mutationMaxBatch = "mutation_max_batch"
        case maxPageSize = "max_page_size"
        case freshnessProbe = "freshness_probe"
    }

    public init() {
        readKinds = []
        saveKinds = []
        followKinds = []
        mutationMaxBatch = nil
        maxPageSize = nil
        freshnessProbe = false
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        readKinds = try container.decodeIfPresent([MediaKind].self, forKey: .readKinds) ?? []
        saveKinds = try container.decodeIfPresent([MediaKind].self, forKey: .saveKinds)
            ?? container.decodeIfPresent([MediaKind].self, forKey: .writeKinds)
            ?? []
        followKinds = try container.decodeIfPresent([MediaKind].self, forKey: .followKinds) ?? []
        mutationMaxBatch = try container.decodeIfPresent(Int.self, forKey: .mutationMaxBatch)
        maxPageSize = try container.decodeIfPresent(Int.self, forKey: .maxPageSize)
        freshnessProbe = try container.decodeIfPresent(Bool.self, forKey: .freshnessProbe) ?? false
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(readKinds, forKey: .readKinds)
        try container.encode(saveKinds, forKey: .saveKinds)
        try container.encode(followKinds, forKey: .followKinds)
        try container.encodeIfPresent(mutationMaxBatch, forKey: .mutationMaxBatch)
        try container.encodeIfPresent(maxPageSize, forKey: .maxPageSize)
        try container.encode(freshnessProbe, forKey: .freshnessProbe)
    }

    static let empty = ProviderLibraryCapabilities()
}

public struct ProviderPlaylistCapabilities: Codable, Sendable, Equatable {
    public let list: Bool
    public let itemRead: Bool
    public let create: Bool
    public let add: Bool
    public let remove: Bool
    public let reorder: Bool
    public let image: Bool
    public let unfollow: Bool
    public let versionTokens: Bool
    public let listMaxPageSize: Int?
    public let itemsMaxPageSize: Int?
    public let addMaxBatch: Int?
    public let removeMaxBatch: Int?

    enum CodingKeys: String, CodingKey {
        case list
        case itemRead = "item_read"
        case create, add, remove, reorder, image, unfollow
        case versionTokens = "version_tokens"
        case listMaxPageSize = "list_max_page_size"
        case itemsMaxPageSize = "items_max_page_size"
        case addMaxBatch = "add_max_batch"
        case removeMaxBatch = "remove_max_batch"
    }

    public init() {
        list = false
        itemRead = false
        create = false
        add = false
        remove = false
        reorder = false
        image = false
        unfollow = false
        versionTokens = false
        listMaxPageSize = nil
        itemsMaxPageSize = nil
        addMaxBatch = nil
        removeMaxBatch = nil
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        list = try container.decodeIfPresent(Bool.self, forKey: .list) ?? false
        itemRead = try container.decodeIfPresent(Bool.self, forKey: .itemRead) ?? false
        create = try container.decodeIfPresent(Bool.self, forKey: .create) ?? false
        add = try container.decodeIfPresent(Bool.self, forKey: .add) ?? false
        remove = try container.decodeIfPresent(Bool.self, forKey: .remove) ?? false
        reorder = try container.decodeIfPresent(Bool.self, forKey: .reorder) ?? false
        image = try container.decodeIfPresent(Bool.self, forKey: .image) ?? false
        unfollow = try container.decodeIfPresent(Bool.self, forKey: .unfollow) ?? false
        versionTokens = try container.decodeIfPresent(Bool.self, forKey: .versionTokens) ?? false
        listMaxPageSize = try container.decodeIfPresent(Int.self, forKey: .listMaxPageSize)
        itemsMaxPageSize = try container.decodeIfPresent(Int.self, forKey: .itemsMaxPageSize)
        addMaxBatch = try container.decodeIfPresent(Int.self, forKey: .addMaxBatch)
        removeMaxBatch = try container.decodeIfPresent(Int.self, forKey: .removeMaxBatch)
    }

    static let empty = ProviderPlaylistCapabilities()
}

public struct ProviderTransportCapabilities: Codable, Sendable, Equatable {
    public let playbackState: Bool
    public let play: Bool
    public let pause: Bool
    public let resume: Bool
    public let next: Bool
    public let previous: Bool
    public let seek: Bool
    public let volume: Bool
    public let shuffle: Bool
    public let repeatMode: Bool
    public let queueRead: Bool
    public let queueAdd: Bool
    public let devices: Bool
    public let transfer: Bool

    enum CodingKeys: String, CodingKey {
        case playbackState = "playback_state"
        case play, pause, resume, next, previous, seek, volume, shuffle
        case repeatMode = "repeat"
        case queueRead = "queue_read"
        case queueAdd = "queue_add"
        case devices, transfer
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        playbackState = try container.decodeIfPresent(Bool.self, forKey: .playbackState) ?? false
        play = try container.decodeIfPresent(Bool.self, forKey: .play) ?? false
        pause = try container.decodeIfPresent(Bool.self, forKey: .pause) ?? false
        resume = try container.decodeIfPresent(Bool.self, forKey: .resume) ?? false
        next = try container.decodeIfPresent(Bool.self, forKey: .next) ?? false
        previous = try container.decodeIfPresent(Bool.self, forKey: .previous) ?? false
        seek = try container.decodeIfPresent(Bool.self, forKey: .seek) ?? false
        volume = try container.decodeIfPresent(Bool.self, forKey: .volume) ?? false
        shuffle = try container.decodeIfPresent(Bool.self, forKey: .shuffle) ?? false
        repeatMode = try container.decodeIfPresent(Bool.self, forKey: .repeatMode) ?? false
        queueRead = try container.decodeIfPresent(Bool.self, forKey: .queueRead) ?? false
        queueAdd = try container.decodeIfPresent(Bool.self, forKey: .queueAdd) ?? false
        devices = try container.decodeIfPresent(Bool.self, forKey: .devices) ?? false
        transfer = try container.decodeIfPresent(Bool.self, forKey: .transfer) ?? false
    }
}

public struct ProviderCapabilities: Codable, Sendable, Equatable {
    public let search: ProviderSearchCapabilities
    public let catalog: ProviderCatalogCapabilities
    public let library: ProviderLibraryCapabilities
    public let playlists: ProviderPlaylistCapabilities
    public let transport: ProviderTransportCapabilities?

    enum CodingKeys: String, CodingKey {
        case search, catalog, library, playlists, transport
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        search = try container.decodeIfPresent(
            ProviderSearchCapabilities.self, forKey: .search) ?? .empty
        catalog = try container.decodeIfPresent(
            ProviderCatalogCapabilities.self, forKey: .catalog) ?? .empty
        library = try container.decodeIfPresent(
            ProviderLibraryCapabilities.self, forKey: .library) ?? .empty
        playlists = try container.decodeIfPresent(
            ProviderPlaylistCapabilities.self, forKey: .playlists) ?? .empty
        transport = try container.decodeIfPresent(
            ProviderTransportCapabilities.self, forKey: .transport)
    }
}

public struct ProviderDescriptor: Codable, Sendable, Equatable, Identifiable {
    public let id: ProviderID
    public let uriScheme: String
    public let displayName: String
    public let capabilities: ProviderCapabilities
    public let isDefault: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case uriScheme = "uri_scheme"
        case displayName = "display_name"
        case capabilities
        case isDefault = "is_default"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(ProviderID.self, forKey: .id)
        uriScheme = try container.decode(String.self, forKey: .uriScheme)
        displayName = try container.decode(String.self, forKey: .displayName)
        capabilities = try container.decode(ProviderCapabilities.self, forKey: .capabilities)
        isDefault = try container.decodeIfPresent(Bool.self, forKey: .isDefault) ?? false
    }
}

public struct ProviderCatalog: Codable, Sendable, Equatable {
    public let defaultProvider: ProviderID?
    public let providers: [ProviderDescriptor]

    enum CodingKeys: String, CodingKey {
        case defaultProvider = "default_provider"
        case providers
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        defaultProvider = try container.decodeIfPresent(ProviderID.self, forKey: .defaultProvider)
        providers = try container.decodeIfPresent([ProviderDescriptor].self, forKey: .providers) ?? []
    }

    public var defaultDescriptor: ProviderDescriptor? {
        guard let defaultProvider else { return nil }
        return providers.first { $0.id == defaultProvider }
    }

    public func provider(forResourceURI uri: String) -> ProviderDescriptor? {
        guard let scheme = uri.split(separator: ":", maxSplits: 1).first else { return nil }
        return providers.first { $0.uriScheme == String(scheme) }
    }
}

public struct ClientPreferences: Codable, Sendable, Equatable {
    public let visualizationColorScheme: String?

    enum CodingKeys: String, CodingKey {
        case visualizationColorScheme = "viz_color_scheme"
    }
}

public struct ResolvedTarget: Codable, Sendable, Equatable {
    public let provider: ProviderID
    public let uri: String
}

public enum SyncCompletionStatus: String, Codable, Sendable, Equatable {
    case succeeded, partial, failed
    /// An unrecognized status from a newer daemon; decodes here instead of
    /// throwing so one unknown value never fails the whole sync summary.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = SyncCompletionStatus(rawValue: raw) ?? .unknown
    }
}

public struct ProviderSyncOutcome: Codable, Sendable, Equatable {
    public let provider: ProviderID
    public let status: SyncCompletionStatus
    public let error: String?
}

public struct CacheSyncSummary: Decodable, Sendable, Equatable {
    public let target: SyncTarget
    public let provider: ProviderID?
    public let playbackSnapshots: UInt32
    public let queueSnapshots: UInt32
    public let queueItems: UInt32
    public let devices: UInt32
    public let playlists: UInt32
    public let playlistItems: UInt32
    public let recentItems: UInt32
    public let libraryItems: UInt32
    public let mediaItems: UInt32
    public let status: SyncCompletionStatus
    public let error: String?
    public let providerOutcomes: [ProviderSyncOutcome]

    enum CodingKeys: String, CodingKey {
        case target, provider, devices, playlists, status, error
        case playbackSnapshots = "playback_snapshots"
        case queueSnapshots = "queue_snapshots"
        case queueItems = "queue_items"
        case playlistItems = "playlist_items"
        case recentItems = "recent_items"
        case libraryItems = "library_items"
        case mediaItems = "media_items"
        case providerOutcomes = "provider_outcomes"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        target = try container.decode(SyncTarget.self, forKey: .target)
        provider = try container.decodeIfPresent(ProviderID.self, forKey: .provider)
        playbackSnapshots = try container.decode(UInt32.self, forKey: .playbackSnapshots)
        queueSnapshots = try container.decodeIfPresent(UInt32.self, forKey: .queueSnapshots) ?? 0
        queueItems = try container.decodeIfPresent(UInt32.self, forKey: .queueItems) ?? 0
        devices = try container.decode(UInt32.self, forKey: .devices)
        playlists = try container.decode(UInt32.self, forKey: .playlists)
        playlistItems = try container.decode(UInt32.self, forKey: .playlistItems)
        recentItems = try container.decode(UInt32.self, forKey: .recentItems)
        libraryItems = try container.decode(UInt32.self, forKey: .libraryItems)
        mediaItems = try container.decode(UInt32.self, forKey: .mediaItems)
        status = try container.decodeIfPresent(SyncCompletionStatus.self, forKey: .status)
            ?? .succeeded
        error = try container.decodeIfPresent(String.self, forKey: .error)
        providerOutcomes = try container.decodeIfPresent(
            [ProviderSyncOutcome].self, forKey: .providerOutcomes) ?? []
    }
}

/// Read-only startup snapshot returned by `client-seed`.
public struct ProviderPolicyNotice: Codable, Sendable, Hashable {
    public let provider: ProviderID
    public let reason: String

    public init(provider: ProviderID, reason: String) {
        self.provider = provider
        self.reason = reason
    }
}

public struct ClientSeed: Decodable, Sendable {
    public let playback: Playback
    public let queue: Queue
    public let devices: [Device]
    public let recent: [MediaItem]
    /// `nil` means an older daemon did not expose capabilities. A present
    /// catalog with no providers means capabilities are explicitly absent.
    public let providerCatalog: ProviderCatalog?
    public let preferences: ClientPreferences?
    /// `nil` means an older daemon omitted policy state. New daemons send an
    /// explicit list so lag/reconnect recovery can reconcile stale notices.
    public let providerPolicies: [ProviderPolicyNotice]?

    enum CodingKeys: String, CodingKey {
        case playback, queue, devices, recent, preferences
        case providerCatalog = "provider_catalog"
        case providerPolicies = "provider_policies"
    }
}
