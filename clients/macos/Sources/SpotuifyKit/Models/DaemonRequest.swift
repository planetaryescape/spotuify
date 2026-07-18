import Foundation

public enum SearchScope: String, Sendable, CaseIterable {
    case all, track, episode, show, album, artist, playlist
}

public enum SearchSource: Codable, Sendable, Equatable, Hashable {
    case local
    case remote(ProviderID)
    case hybrid

    /// Source used by released v7 clients. It remains a scalar on the wire.
    public static let spotify = SearchSource.remote(.spotify)

    public init(from decoder: Decoder) throws {
        let singleValue = try decoder.singleValueContainer()
        if let scalar = try? singleValue.decode(String.self) {
            switch scalar {
            case "local": self = .local
            case "hybrid": self = .hybrid
            case "spotify": self = .remote(.spotify)
            default:
                throw DecodingError.dataCorruptedError(
                    in: singleValue,
                    debugDescription: "Unknown search source \(scalar)")
            }
            return
        }

        let container = try decoder.container(keyedBy: AnyKey.self)
        let remoteKey = AnyKey("remote")
        guard container.allKeys.map(\.stringValue) == [remoteKey.stringValue] else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath,
                      debugDescription: "Remote search source must contain only remote"))
        }
        self = .remote(try container.decode(ProviderID.self, forKey: remoteKey))
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .local:
            var container = encoder.singleValueContainer()
            try container.encode("local")
        case .hybrid:
            var container = encoder.singleValueContainer()
            try container.encode("hybrid")
        case .remote(let provider) where provider == .spotify:
            var container = encoder.singleValueContainer()
            try container.encode("spotify")
        case .remote(let provider):
            var container = encoder.container(keyedBy: AnyKey.self)
            try container.encode(provider, forKey: AnyKey("remote"))
        }
    }
}

public enum SearchSort: String, Sendable, CaseIterable {
    case relevance, name, duration, artist, date
}

/// How the cross-show episode feed is ordered (mirrors protocol `EpisodeSort`).
public enum EpisodeSort: String, Sendable, CaseIterable {
    case newest, oldest, duration, title, show
}

public enum RepeatMode: String, Sendable, CaseIterable {
    case off, context, track
}

/// `analytics top` grouping (mirrors protocol `TopKind`, snake_case).
public enum AnalyticsTopKind: String, Sendable, CaseIterable {
    case tracks, artists, albums, playlists
}

/// `analytics habits` bucket (mirrors core `HabitWindow`, snake_case).
public enum AnalyticsHabitWindow: String, Sendable, CaseIterable {
    case day, week, month
}

/// `analytics search` redaction mode (mirrors `SearchMode`, snake_case).
public enum AnalyticsSearchMode: String, Sendable, CaseIterable {
    case raw, normalized
}

/// `analytics top` time window. Wire shape is `{ "days": N }` or `"all"`
/// (externally-tagged serde enum).
public enum AnalyticsSinceWindow: Sendable, Encodable {
    case days(UInt32)
    case all

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .all:
            var c = encoder.singleValueContainer()
            try c.encode("all")
        case .days(let n):
            var c = encoder.container(keyedBy: AnyKey.self)
            try c.encode(n, forKey: AnyKey("days"))
        }
    }
}

/// Visualizer capture source (mirrors `VizSourceKindData`, lowercase).
public enum VizSourceKind: String, Sendable, CaseIterable {
    case auto, sink, loopback, none
}

/// Sync target (mirrors `SyncTargetData`, lowercase).
public enum SyncTarget: String, Codable, Sendable, CaseIterable {
    case all, playback, queue, devices, playlists, recent, library
}

/// Origin attributed to a recorded operation (mirrors `OperationSource`,
/// kebab-case).
public enum OperationSource: String, Sendable, CaseIterable {
    case cli, tui, mcp, agent
    case daemonInternal = "daemon-internal"
}

public enum PlaylistItemMutationAction: String, Sendable, CaseIterable {
    case add, remove
}

/// A playback mutation. Externally tagged kebab-case on the wire: unit cases
/// serialize as a bare string ("pause"), data cases as a single-key object
/// (`{"seek":{"position_ms":N}}`).
public enum PlaybackCommand: Encodable, Sendable {
    case pause, resume, toggle, next, previous
    /// Play a track/context, optionally starting inside a collection
    /// `contextURI` (album/playlist URI, or the Liked-Songs sentinel).
    /// `contextURI` is omitted from the wire when nil so the JSON matches
    /// the daemon's `#[serde(default, skip_serializing_if)]` form exactly.
    case playURI(String, contextURI: String? = nil)
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
        case .playURI(let uri, let contextURI):
            try encodeObject(encoder, tag: "play-uri") {
                try $0.encode(uri, forKey: AnyKey("uri"))
                if let contextURI {
                    try $0.encode(contextURI, forKey: AnyKey("context_uri"))
                }
            }
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
    case getDaemonStatus
    case subscribeEvents
    case clientSeed
    case providersList
    case resolveTarget(
        input: String, provider: ProviderID? = nil, expectedKinds: [MediaKind]? = nil)
    case listAudioOutputs
    case playbackGet
    case queueGet
    case devicesList
    case playlistsList(provider: ProviderID? = nil)
    case recentlyPlayed(provider: ProviderID? = nil)
    case libraryList(limit: UInt32, provider: ProviderID? = nil)
    case playbackCommand(PlaybackCommand)
    case deviceTransfer(device: String)
    case search(
        query: String, scope: SearchScope, source: SearchSource, limit: UInt32,
        provider: ProviderID? = nil, kinds: [MediaKind]? = nil, sort: SearchSort? = nil)
    case searchStream(
        query: String, scope: SearchScope, source: SearchSource, version: UInt64,
        provider: ProviderID? = nil)
    case searchPage(
        query: String, kind: MediaKind, offset: UInt32, version: UInt64,
        provider: ProviderID? = nil)
    case queueAdd(uri: String)
    case queueAddMany(uris: [String])
    case savedTracks(limit: UInt32, offset: UInt32, provider: ProviderID? = nil)
    case savedShows(limit: UInt32, provider: ProviderID? = nil)
    case showEpisodes(show: String, limit: UInt32, offset: UInt32)
    case albumTracks(album: String)
    case artistAlbums(artist: String)
    case followedArtists(limit: UInt32, provider: ProviderID? = nil)
    case artistFollow(artist: String)
    case artistUnfollow(artist: String)
    case listenSessions(limit: UInt32)
    case playlistTracks(playlist: String, wait: Bool, provider: ProviderID? = nil)
    case playlistAddItems(playlist: String, uris: [String], provider: ProviderID? = nil)
    case librarySave(uri: String?, current: Bool)
    case libraryUnsave(uri: String)
    case lyricsGet(trackURI: String?, forceRefresh: Bool)
    case lyricsOffsetSet(trackURI: String, offsetMs: Int64)
    case coverArt(url: String)
    case setVizEnabled(Bool)
    case reminderCreate(uri: String, anchorAtMs: Int64, recurrence: Recurrence, tz: String, message: String?)
    case remindersList(includeInactive: Bool)
    case reminderCancel(id: String)
    case notificationsList(includeArchived: Bool)
    case notificationAct(id: String, action: String, snoozeUntilMs: Int64?)
    case checkUpdate(force: Bool)
    case episodeFeed(
        limit: UInt32, sort: EpisodeSort, refresh: Bool, provider: ProviderID? = nil)
    // --- admin / maintenance ---
    case shutdown
    case getDoctorReport
    case reindex
    case cacheStatus
    case logsTail(lines: UInt64)
    case sync(target: SyncTarget, provider: ProviderID? = nil)
    case image(url: String)
    case reconnect
    case setAudioOutput(device: String? = nil)
    case reload
    case reloadAuth
    case authStart(provider: ProviderID? = nil, method: String? = nil)
    case authPoll(sessionId: UUID)
    case authCancel(sessionId: UUID)
    case authStatus(provider: ProviderID? = nil)
    case authLogout(provider: ProviderID? = nil)
    case webApiToken(force: Bool)
    case searchCachePrune(olderThanMs: Int64? = nil)
    // --- playlist mutations ---
    case playlistCreate(
        name: String, description: String? = nil, uris: [String],
        provider: ProviderID? = nil)
    case playlistCreatePreview(
        name: String, description: String? = nil, uris: [String],
        provider: ProviderID? = nil)
    case playlistItemsPreview(
        playlist: String, uris: [String], action: PlaylistItemMutationAction,
        provider: ProviderID? = nil)
    case playlistRemoveItems(playlist: String, uris: [String], provider: ProviderID? = nil)
    case playlistSetImage(
        playlist: String, imageBase64: String, provider: ProviderID? = nil)
    case playlistUnfollow(playlist: String, provider: ProviderID? = nil)
    // --- visualizer ---
    case getVizStatus
    case setVizSource(kind: VizSourceKind)
    case setVizFocus(focused: Bool)
    // --- operations log ---
    case opsLog(limit: UInt32, sinceMs: Int64? = nil, source: OperationSource? = nil)
    case opsShow(operationId: String, withDiff: Bool)
    case opsUndo(
        operationId: String? = nil, dryRun: Bool = false, force: Bool = false,
        bulkSinceMs: Int64? = nil)
    case opsRedo(operationId: String? = nil)
    // --- analytics ---
    case analyticsTop(kind: AnalyticsTopKind, sinceWindow: AnalyticsSinceWindow, limit: UInt32)
    case analyticsHabits(window: AnalyticsHabitWindow, sinceMs: Int64? = nil)
    case analyticsSearch(mode: AnalyticsSearchMode, limit: UInt32)
    case analyticsRediscovery(gapDays: UInt32)
    case analyticsRebuild(sinceMs: Int64? = nil)
    case analyticsPrune(apply: Bool)
    // --- Mercury-backed discovery ---
    case relatedArtists(artist: String)
    case radioStart(seedUri: String, dryRun: Bool = false)

    var requiresMutationId: Bool {
        switch self {
        case .opsUndo(_, let dryRun, _, _), .radioStart(_, let dryRun):
            !dryRun
        case .opsRedo:
            true
        case .queueAdd, .queueAddMany,
             .playlistAddItems, .playlistRemoveItems, .playlistCreate,
             .playlistUnfollow, .playlistSetImage,
             .librarySave, .libraryUnsave,
             .artistFollow, .artistUnfollow:
            true
        default:
            false
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: AnyKey.self)
        try c.encode("Request", forKey: AnyKey("type"))
        switch self {
        case .ping:
            try c.encode("ping", forKey: AnyKey("cmd"))
        case .getDaemonStatus:
            try c.encode("get-daemon-status", forKey: AnyKey("cmd"))
        case .subscribeEvents:
            try c.encode("subscribe-events", forKey: AnyKey("cmd"))
            try c.encode(true, forKey: AnyKey("provider_policy"))
        case .clientSeed:
            try c.encode("client-seed", forKey: AnyKey("cmd"))
        case .providersList:
            try c.encode("providers-list", forKey: AnyKey("cmd"))
        case .resolveTarget(let input, let provider, let expectedKinds):
            try c.encode("resolve-target", forKey: AnyKey("cmd"))
            try c.encode(input, forKey: AnyKey("input"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
            try c.encodeIfPresent(expectedKinds, forKey: AnyKey("expected_kinds"))
        case .listAudioOutputs:
            try c.encode("list-audio-outputs", forKey: AnyKey("cmd"))
        case .playbackGet:
            try c.encode("playback-get", forKey: AnyKey("cmd"))
        case .queueGet:
            try c.encode("queue-get", forKey: AnyKey("cmd"))
        case .devicesList:
            try c.encode("devices-list", forKey: AnyKey("cmd"))
        case .playlistsList(let provider):
            try c.encode("playlists-list", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .recentlyPlayed(let provider):
            try c.encode("recently-played", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .libraryList(let limit, let provider):
            try c.encode("library-list", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playbackCommand(let cmd):
            try c.encode("playback-command", forKey: AnyKey("cmd"))
            try c.encode(cmd, forKey: AnyKey("command"))
        case .deviceTransfer(let device):
            try c.encode("device-transfer", forKey: AnyKey("cmd"))
            try c.encode(device, forKey: AnyKey("device"))
        case .search(let query, let scope, let source, let limit, let provider, let kinds, let sort):
            try c.encode("search", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(scope.rawValue, forKey: AnyKey("scope"))
            try c.encode(source, forKey: AnyKey("source"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
            if let kinds {
                try c.encode(kinds.map(\.rawValue), forKey: AnyKey("kinds"))
            }
            if let sort {
                try c.encode(sort.rawValue, forKey: AnyKey("sort"))
            }
        case .searchStream(let query, let scope, let source, let version, let provider):
            try c.encode("search-stream", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(scope.rawValue, forKey: AnyKey("scope"))
            try c.encode(source, forKey: AnyKey("source"))
            try c.encode(version, forKey: AnyKey("version"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .searchPage(let query, let kind, let offset, let version, let provider):
            try c.encode("search-page", forKey: AnyKey("cmd"))
            try c.encode(query, forKey: AnyKey("query"))
            try c.encode(kind.rawValue, forKey: AnyKey("kind"))
            try c.encode(offset, forKey: AnyKey("offset"))
            try c.encode(version, forKey: AnyKey("version"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .queueAdd(let uri):
            try c.encode("queue-add", forKey: AnyKey("cmd"))
            try c.encode(uri, forKey: AnyKey("uri"))
        case .queueAddMany(let uris):
            try c.encode("queue-add-many", forKey: AnyKey("cmd"))
            try c.encode(uris, forKey: AnyKey("uris"))
        case .savedTracks(let limit, let offset, let provider):
            try c.encode("saved-tracks", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encode(offset, forKey: AnyKey("offset"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .savedShows(let limit, let provider):
            try c.encode("saved-shows", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
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
        case .followedArtists(let limit, let provider):
            try c.encode("followed-artists", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .artistFollow(let artist):
            try c.encode("artist-follow", forKey: AnyKey("cmd"))
            try c.encode(artist, forKey: AnyKey("artist"))
        case .artistUnfollow(let artist):
            try c.encode("artist-unfollow", forKey: AnyKey("cmd"))
            try c.encode(artist, forKey: AnyKey("artist"))
        case .listenSessions(let limit):
            try c.encode("listen-sessions", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .playlistTracks(let playlist, let wait, let provider):
            try c.encode("playlist-tracks", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(wait, forKey: AnyKey("wait"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistAddItems(let playlist, let uris, let provider):
            try c.encode("playlist-add-items", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(uris, forKey: AnyKey("uris"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
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
        case .coverArt(let url):
            try c.encode("cover-art", forKey: AnyKey("cmd"))
            try c.encode(url, forKey: AnyKey("url"))
        case .setVizEnabled(let enabled):
            try c.encode("set-viz-enabled", forKey: AnyKey("cmd"))
            try c.encode(enabled, forKey: AnyKey("enabled"))
        case .reminderCreate(let uri, let anchorAtMs, let recurrence, let tz, let message):
            try c.encode("reminder-create", forKey: AnyKey("cmd"))
            try c.encode(uri, forKey: AnyKey("media_uri"))
            try c.encode(anchorAtMs, forKey: AnyKey("anchor_at_ms"))
            try c.encode(recurrence.rawValue, forKey: AnyKey("recurrence"))
            try c.encode(tz, forKey: AnyKey("tz"))
            try c.encodeIfPresent(message, forKey: AnyKey("message"))
        case .remindersList(let includeInactive):
            try c.encode("reminders-list", forKey: AnyKey("cmd"))
            try c.encode(includeInactive, forKey: AnyKey("include_inactive"))
        case .reminderCancel(let id):
            try c.encode("reminder-cancel", forKey: AnyKey("cmd"))
            try c.encode(id, forKey: AnyKey("id"))
        case .notificationsList(let includeArchived):
            try c.encode("notifications-list", forKey: AnyKey("cmd"))
            try c.encode(includeArchived, forKey: AnyKey("include_archived"))
        case .notificationAct(let id, let action, let snoozeUntilMs):
            try c.encode("notification-act", forKey: AnyKey("cmd"))
            try c.encode(id, forKey: AnyKey("id"))
            try c.encode(action, forKey: AnyKey("action"))
            try c.encodeIfPresent(snoozeUntilMs, forKey: AnyKey("snooze_until_ms"))
        case .checkUpdate(let force):
            try c.encode("check-update", forKey: AnyKey("cmd"))
            try c.encode(force, forKey: AnyKey("force"))
        case .episodeFeed(let limit, let sort, let refresh, let provider):
            try c.encode("episode-feed", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encode(sort.rawValue, forKey: AnyKey("sort"))
            try c.encode(refresh, forKey: AnyKey("refresh"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .shutdown:
            try c.encode("shutdown", forKey: AnyKey("cmd"))
        case .getDoctorReport:
            try c.encode("get-doctor-report", forKey: AnyKey("cmd"))
        case .reindex:
            try c.encode("reindex", forKey: AnyKey("cmd"))
        case .cacheStatus:
            try c.encode("cache-status", forKey: AnyKey("cmd"))
        case .logsTail(let lines):
            try c.encode("logs-tail", forKey: AnyKey("cmd"))
            try c.encode(lines, forKey: AnyKey("lines"))
        case .sync(let target, let provider):
            try c.encode("sync", forKey: AnyKey("cmd"))
            try c.encode(target.rawValue, forKey: AnyKey("target"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .image(let url):
            try c.encode("image", forKey: AnyKey("cmd"))
            try c.encode(url, forKey: AnyKey("url"))
        case .reconnect:
            try c.encode("reconnect", forKey: AnyKey("cmd"))
        case .setAudioOutput(let device):
            try c.encode("set-audio-output", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(device, forKey: AnyKey("device"))
        case .reload:
            try c.encode("reload", forKey: AnyKey("cmd"))
        case .reloadAuth:
            try c.encode("reload-auth", forKey: AnyKey("cmd"))
        case .authStart(let provider, let method):
            try c.encode("auth-start", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
            try c.encodeIfPresent(method, forKey: AnyKey("method"))
        case .authPoll(let sessionId):
            try c.encode("auth-poll", forKey: AnyKey("cmd"))
            try c.encode(sessionId, forKey: AnyKey("session_id"))
        case .authCancel(let sessionId):
            try c.encode("auth-cancel", forKey: AnyKey("cmd"))
            try c.encode(sessionId, forKey: AnyKey("session_id"))
        case .authStatus(let provider):
            try c.encode("auth-status", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .authLogout(let provider):
            try c.encode("auth-logout", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .webApiToken(let force):
            try c.encode("web-api-token", forKey: AnyKey("cmd"))
            try c.encode(force, forKey: AnyKey("force"))
        case .searchCachePrune(let olderThanMs):
            try c.encode("search-cache-prune", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(olderThanMs, forKey: AnyKey("older_than_ms"))
        case .playlistCreate(let name, let description, let uris, let provider):
            try c.encode("playlist-create", forKey: AnyKey("cmd"))
            try c.encode(name, forKey: AnyKey("name"))
            try c.encodeIfPresent(description, forKey: AnyKey("description"))
            try c.encode(uris, forKey: AnyKey("uris"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistCreatePreview(let name, let description, let uris, let provider):
            try c.encode("playlist-create-preview", forKey: AnyKey("cmd"))
            try c.encode(name, forKey: AnyKey("name"))
            try c.encodeIfPresent(description, forKey: AnyKey("description"))
            try c.encode(uris, forKey: AnyKey("uris"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistItemsPreview(let playlist, let uris, let action, let provider):
            try c.encode("playlist-items-preview", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(uris, forKey: AnyKey("uris"))
            try c.encode(action.rawValue, forKey: AnyKey("action"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistRemoveItems(let playlist, let uris, let provider):
            try c.encode("playlist-remove-items", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(uris, forKey: AnyKey("uris"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistSetImage(let playlist, let imageBase64, let provider):
            try c.encode("playlist-set-image", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encode(imageBase64, forKey: AnyKey("image_base64"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .playlistUnfollow(let playlist, let provider):
            try c.encode("playlist-unfollow", forKey: AnyKey("cmd"))
            try c.encode(playlist, forKey: AnyKey("playlist"))
            try c.encodeIfPresent(provider, forKey: AnyKey("provider"))
        case .getVizStatus:
            try c.encode("get-viz-status", forKey: AnyKey("cmd"))
        case .setVizSource(let kind):
            try c.encode("set-viz-source", forKey: AnyKey("cmd"))
            try c.encode(kind.rawValue, forKey: AnyKey("kind"))
        case .setVizFocus(let focused):
            try c.encode("set-viz-focus", forKey: AnyKey("cmd"))
            try c.encode(focused, forKey: AnyKey("focused"))
        case .opsLog(let limit, let sinceMs, let source):
            try c.encode("ops-log", forKey: AnyKey("cmd"))
            try c.encode(limit, forKey: AnyKey("limit"))
            try c.encodeIfPresent(sinceMs, forKey: AnyKey("since_ms"))
            try c.encodeIfPresent(source?.rawValue, forKey: AnyKey("source"))
        case .opsShow(let operationId, let withDiff):
            try c.encode("ops-show", forKey: AnyKey("cmd"))
            try c.encode(operationId, forKey: AnyKey("operation_id"))
            try c.encode(withDiff, forKey: AnyKey("with_diff"))
        case .opsUndo(let operationId, let dryRun, let force, let bulkSinceMs):
            try c.encode("ops-undo", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(operationId, forKey: AnyKey("operation_id"))
            try c.encode(dryRun, forKey: AnyKey("dry_run"))
            try c.encode(force, forKey: AnyKey("force"))
            try c.encodeIfPresent(bulkSinceMs, forKey: AnyKey("bulk_since_ms"))
        case .opsRedo(let operationId):
            try c.encode("ops-redo", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(operationId, forKey: AnyKey("operation_id"))
        case .analyticsTop(let kind, let sinceWindow, let limit):
            try c.encode("analytics-top", forKey: AnyKey("cmd"))
            try c.encode(kind.rawValue, forKey: AnyKey("kind"))
            try c.encode(sinceWindow, forKey: AnyKey("since_window"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .analyticsHabits(let window, let sinceMs):
            try c.encode("analytics-habits", forKey: AnyKey("cmd"))
            try c.encode(window.rawValue, forKey: AnyKey("window"))
            try c.encodeIfPresent(sinceMs, forKey: AnyKey("since_ms"))
        case .analyticsSearch(let mode, let limit):
            try c.encode("analytics-search", forKey: AnyKey("cmd"))
            try c.encode(mode.rawValue, forKey: AnyKey("mode"))
            try c.encode(limit, forKey: AnyKey("limit"))
        case .analyticsRediscovery(let gapDays):
            try c.encode("analytics-rediscovery", forKey: AnyKey("cmd"))
            try c.encode(gapDays, forKey: AnyKey("gap_days"))
        case .analyticsRebuild(let sinceMs):
            try c.encode("analytics-rebuild", forKey: AnyKey("cmd"))
            try c.encodeIfPresent(sinceMs, forKey: AnyKey("since_ms"))
        case .analyticsPrune(let apply):
            try c.encode("analytics-prune", forKey: AnyKey("cmd"))
            try c.encode(apply, forKey: AnyKey("apply"))
        case .relatedArtists(let artist):
            try c.encode("related-artists", forKey: AnyKey("cmd"))
            try c.encode(artist, forKey: AnyKey("artist"))
        case .radioStart(let seedUri, let dryRun):
            try c.encode("radio-start", forKey: AnyKey("cmd"))
            try c.encode(seedUri, forKey: AnyKey("seed_uri"))
            try c.encode(dryRun, forKey: AnyKey("dry_run"))
        }
    }

    /// One sample per case, used by the protocol-parity test to extract
    /// the full set of `cmd` strings this client can emit and compare it
    /// against the Rust roster fixture. Exhaustiveness is enforced by the
    /// parity test (a missing case shows up as a roster mismatch), not by
    /// the compiler.
    public static var allSamples: [DaemonRequest] {
        [
            .ping, .getDaemonStatus, .subscribeEvents, .clientSeed,
            .providersList, .resolveTarget(input: "u"), .listAudioOutputs, .playbackGet,
            .queueGet, .devicesList, .playlistsList(), .recentlyPlayed(),
            .libraryList(limit: 1), .playbackCommand(.pause), .deviceTransfer(device: "d"),
            .search(query: "q", scope: .track, source: .local, limit: 1),
            .searchStream(query: "q", scope: .track, source: .local, version: 1),
            .searchPage(query: "q", kind: .track, offset: 0, version: 1),
            .queueAdd(uri: "u"), .queueAddMany(uris: ["u"]),
            .savedTracks(limit: 1, offset: 0), .savedShows(limit: 1),
            .showEpisodes(show: "s", limit: 1, offset: 0), .albumTracks(album: "a"),
            .artistAlbums(artist: "a"), .followedArtists(limit: 1),
            .artistFollow(artist: "a"), .artistUnfollow(artist: "a"),
            .listenSessions(limit: 1), .playlistTracks(playlist: "p", wait: false),
            .playlistAddItems(playlist: "p", uris: ["u"]),
            .librarySave(uri: nil, current: true), .libraryUnsave(uri: "u"),
            .lyricsGet(trackURI: nil, forceRefresh: false),
            .lyricsOffsetSet(trackURI: "u", offsetMs: 0), .coverArt(url: "u"),
            .setVizEnabled(true),
            .reminderCreate(uri: "u", anchorAtMs: 0, recurrence: .none, tz: "UTC", message: nil),
            .remindersList(includeInactive: false), .reminderCancel(id: "i"),
            .notificationsList(includeArchived: false),
            .notificationAct(id: "i", action: "a", snoozeUntilMs: nil),
            .checkUpdate(force: false), .episodeFeed(limit: 1, sort: .newest, refresh: false),
            .shutdown, .getDoctorReport, .reindex, .cacheStatus, .logsTail(lines: 1),
            .sync(target: .all), .image(url: "u"), .reconnect, .setAudioOutput(device: nil),
            .reload, .reloadAuth,
            .authStart(provider: nil, method: nil),
            .authPoll(sessionId: UUID()), .authCancel(sessionId: UUID()),
            .authStatus(provider: nil), .authLogout(provider: nil),
            .webApiToken(force: false), .searchCachePrune(olderThanMs: nil),
            .playlistCreate(name: "n", description: nil, uris: ["u"]),
            .playlistCreatePreview(name: "n", uris: ["u"]),
            .playlistItemsPreview(playlist: "p", uris: ["u"], action: .remove),
            .playlistRemoveItems(playlist: "p", uris: ["u"]),
            .playlistSetImage(playlist: "p", imageBase64: "x"),
            .playlistUnfollow(playlist: "p"),
            .getVizStatus, .setVizSource(kind: .auto), .setVizFocus(focused: true),
            .opsLog(limit: 1, sinceMs: nil, source: nil),
            .opsShow(operationId: "i", withDiff: false), .opsUndo(), .opsRedo(),
            .analyticsTop(kind: .tracks, sinceWindow: .days(30), limit: 1),
            .analyticsHabits(window: .week, sinceMs: nil),
            .analyticsSearch(mode: .raw, limit: 1),
            .analyticsRediscovery(gapDays: 90), .analyticsRebuild(sinceMs: nil),
            .analyticsPrune(apply: false),
            .relatedArtists(artist: "spotify:artist:1"),
            .radioStart(seedUri: "spotify:track:1", dryRun: false),
        ]
    }

    /// The `cmd` string this request encodes to. Decoding our own
    /// encoded form keeps it in lockstep with `encode(to:)` rather than
    /// duplicating the kebab table.
    public var commandName: String {
        let data = (try? JSONEncoder().encode(self)) ?? Data()
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        return (obj?["cmd"] as? String) ?? ""
    }
}
