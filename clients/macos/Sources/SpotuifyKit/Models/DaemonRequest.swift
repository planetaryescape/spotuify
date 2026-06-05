import Foundation

public enum SearchScope: String, Sendable, CaseIterable {
    case all, track, episode, show, album, artist, playlist
}

public enum SearchSource: String, Sendable {
    case local, spotify, hybrid
}

public enum RepeatMode: String, Sendable, CaseIterable {
    case off, context, track
}

/// A playback mutation. Externally tagged kebab-case on the wire: unit cases
/// serialize as a bare string ("pause"), data cases as a single-key object
/// (`{"seek":{"position_ms":N}}`).
public enum PlaybackCommand: Encodable, Sendable {
    case pause, resume, toggle, next, previous
    case playURI(String)
    case seek(positionMs: UInt64)
    case seekRelative(offsetMs: Int64)
    case volume(percent: UInt8)
    case shuffle(Bool)
    case repeatMode(RepeatMode)

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .pause: try encodeUnit(encoder, "pause")
        case .resume: try encodeUnit(encoder, "resume")
        case .toggle: try encodeUnit(encoder, "toggle")
        case .next: try encodeUnit(encoder, "next")
        case .previous: try encodeUnit(encoder, "previous")
        case .playURI(let uri):
            try encodeObject(encoder, tag: "play-uri") { try $0.encode(uri, forKey: AnyKey("uri")) }
        case .seek(let ms):
            try encodeObject(encoder, tag: "seek") { try $0.encode(ms, forKey: AnyKey("position_ms")) }
        case .seekRelative(let off):
            try encodeObject(encoder, tag: "seek-relative") { try $0.encode(off, forKey: AnyKey("offset_ms")) }
        case .volume(let percent):
            try encodeObject(encoder, tag: "volume") { try $0.encode(percent, forKey: AnyKey("volume_percent")) }
        case .shuffle(let state):
            try encodeObject(encoder, tag: "shuffle") { try $0.encode(state, forKey: AnyKey("state")) }
        case .repeatMode(let mode):
            try encodeObject(encoder, tag: "repeat") { try $0.encode(mode.rawValue, forKey: AnyKey("state")) }
        }
    }

    private func encodeUnit(_ encoder: Encoder, _ tag: String) throws {
        var c = encoder.singleValueContainer()
        try c.encode(tag)
    }

    private func encodeObject(
        _ encoder: Encoder,
        tag: String,
        _ body: (inout KeyedEncodingContainer<AnyKey>) throws -> Void
    ) throws {
        var outer = encoder.container(keyedBy: AnyKey.self)
        var inner = outer.nestedContainer(keyedBy: AnyKey.self, forKey: AnyKey(tag))
        try body(&inner)
    }
}

/// Outbound requests. `encode(to:)` writes `{"type":"Request","cmd":<kebab>,…}`
/// directly (serde `#[serde(tag="cmd", rename_all="kebab-case")]` shape).
public enum DaemonRequest: Encodable, Sendable {
    case ping
    case subscribeEvents
    case clientSeed
    case playbackGet
    case queueGet
    case devicesList
    case playlistsList
    case recentlyPlayed
    case libraryList(limit: UInt32)
    case playbackCommand(PlaybackCommand)
    case deviceTransfer(device: String)
    case search(query: String, scope: SearchScope, source: SearchSource, limit: UInt32)
    case searchStream(query: String, scope: SearchScope, source: SearchSource, version: UInt64)
    case searchPage(query: String, kind: MediaKind, offset: UInt32, version: UInt64)
    case queueAdd(uri: String)
    case queueAddMany(uris: [String])
    case savedTracks(limit: UInt32, offset: UInt32)
    case savedShows(limit: UInt32)
    case showEpisodes(show: String, limit: UInt32, offset: UInt32)
    case albumTracks(album: String)
    case artistAlbums(artist: String)
    case playlistTracks(playlist: String, wait: Bool)
    case playlistAddItems(playlist: String, uris: [String])
    case librarySave(uri: String?, current: Bool)
    case libraryUnsave(uri: String)
    case lyricsGet(trackURI: String?, forceRefresh: Bool)
    case lyricsOffsetSet(trackURI: String, offsetMs: Int64)
    case setVizEnabled(Bool)

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: AnyKey.self)
        try c.encode("Request", forKey: AnyKey("type"))
        switch self {
        case .ping:
            try c.encode("ping", forKey: AnyKey("cmd"))
        case .subscribeEvents:
            try c.encode("subscribe-events", forKey: AnyKey("cmd"))
        case .clientSeed:
            try c.encode("client-seed", forKey: AnyKey("cmd"))
        case .playbackGet:
            try c.encode("playback-get", forKey: AnyKey("cmd"))
        case .queueGet:
            try c.encode("queue-get", forKey: AnyKey("cmd"))
        case .devicesList:
            try c.encode("devices-list", forKey: AnyKey("cmd"))
        case .playlistsList:
            try c.encode("playlists-list", forKey: AnyKey("cmd"))
        case .recentlyPlayed:
            try c.encode("recently-played", forKey: AnyKey("cmd"))
        case .libraryList(let limit):
            try c.encode("library-list", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .playbackCommand(let cmd):
            try c.encode("playback-command", forKey: AnyKey("cmd"))
            try c.encode(cmd, forKey: AnyKey("command"))
        case .deviceTransfer(let device):
            try c.encode("device-transfer", forKey: AnyKey("cmd"))
            try c.encode(device, forKey: AnyKey("device"))
        case .search(let query, let scope, let source, let limit):
            try c.encode("search", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(scope.rawValue, forKey: AnyKey("scope"))
            try c.encode(source.rawValue, forKey: AnyKey("source"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .searchStream(let query, let scope, let source, let version):
            try c.encode("search-stream", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(scope.rawValue, forKey: AnyKey("scope"))
            try c.encode(source.rawValue, forKey: AnyKey("source"))
            try c.encode(version, forKey: AnyKey("version"))
        case .searchPage(let query, let kind, let offset, let version):
            try c.encode("search-page", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(kind.rawValue, forKey: AnyKey("kind"))
            try c.encode(offset, forKey: AnyKey("offset"))
            try c.encode(version, forKey: AnyKey("version"))
        case .queueAdd(let uri):
            try c.encode("queue-add", forKey: AnyKey("cmd"))
            try c.encode(uri, forKey: AnyKey("uri"))
        case .queueAddMany(let uris):
            try c.encode("queue-add-many", forKey: AnyKey("cmd"))
            try c.encode(uris, forKey: AnyKey("uris"))
        case .savedTracks(let limit, let offset):
            try c.encode("saved-tracks", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encode(offset, forKey: AnyKey("offset"))
        case .savedShows(let limit):
            try c.encode("saved-shows", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .showEpisodes(let show, let limit, let offset):
            try c.encode("show-episodes", forKey: AnyKey("cmd"))
            try c.encode(show, forKey: AnyKey("show"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encode(offset, forKey: AnyKey("offset"))
        case .albumTracks(let album):
            try c.encode("album-tracks", forKey: AnyKey("cmd"))
            try c.encode(album, forKey: AnyKey("album"))
        case .artistAlbums(let artist):
            try c.encode("artist-albums", forKey: AnyKey("cmd"))
            try c.encode(artist, forKey: AnyKey("artist"))
        case .playlistTracks(let playlist, let wait):
            try c.encode("playlist-tracks", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(wait, forKey: AnyKey("wait"))
        case .playlistAddItems(let playlist, let uris):
            try c.encode("playlist-add-items", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(uris, forKey: AnyKey("uris"))
        case .librarySave(let uri, let current):
            try c.encode("library-save", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(uri, forKey: AnyKey("uri"))
            try c.encode(current, forKey: AnyKey("current"))
        case .libraryUnsave(let uri):
            try c.encode("library-unsave", forKey: AnyKey("cmd"))
            try c.encode(uri, forKey: AnyKey("uri"))
        case .lyricsGet(let trackURI, let forceRefresh):
            try c.encode("lyrics-get", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(trackURI, forKey: AnyKey("track_uri"))
            try c.encode(forceRefresh, forKey: AnyKey("force_refresh"))
        case .lyricsOffsetSet(let trackURI, let offsetMs):
            try c.encode("lyrics-offset-set", forKey: AnyKey("cmd"))
            try c.encode(trackURI, forKey: AnyKey("track_uri"))
            try c.encode(offsetMs, forKey: AnyKey("offset_ms"))
        case .setVizEnabled(let enabled):
            try c.encode("set-viz-enabled", forKey: AnyKey("cmd"))
            try c.encode(enabled, forKey: AnyKey("enabled"))
        }
    }
}
