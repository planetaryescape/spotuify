import Foundation

/// A `Response` payload, externally tagged by the daemon as `Ok` / `Error`.
public enum ResponsePayload: Decodable, Sendable {
    case ok(ResponseData)
    case error(DaemonError)

    private enum CodingKeys: String, CodingKey {
        case ok = "Ok"
        case err = "Error"
    }
    private enum OkKeys: String, CodingKey { case data }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if container.contains(.ok) {
            let ok = try container.nestedContainer(keyedBy: OkKeys.self, forKey: .ok)
            self = .ok(try ok.decode(ResponseData.self, forKey: .data))
        } else {
            self = .error(try container.decode(DaemonError.self, forKey: .err))
        }
    }
}

public struct DaemonError: Decodable, Error, Sendable {
    public let message: String
    public let kind: String
    public let code: String?
    public let retryable: Bool?

    /// True when the daemon signalled the Spotify refresh token is gone and
    /// the user must re-authenticate (no client token handling — just surface it).
    public var isAuthRevoked: Bool { kind == "auth_revoked" || code == "auth_revoked" }
}

public struct CommandReceipt: Decodable, Sendable {
    public let ok: Bool
    public let action: String
    public let message: String
}

public struct DaemonStatus: Decodable, Sendable {
    public let running: Bool
    public let protocolVersion: Int
    public let daemonVersion: String?
    public let daemonPid: UInt32?

    enum CodingKeys: String, CodingKey {
        case running
        case protocolVersion = "protocol_version"
        case daemonVersion = "daemon_version"
        case daemonPid = "daemon_pid"
    }
}

/// How this install upgrades (mirrors protocol `UpgradeHint`).
public struct UpgradeHint: Decodable, Sendable {
    /// homebrew | cargo | macapp | manual | dev
    public let method: String
    public let command: String?
    public let url: String?
}

/// Result of `check-update`: whether a newer release exists + how to upgrade.
public struct UpdateStatus: Decodable, Sendable {
    public let updateAvailable: Bool
    public let currentVersion: String
    public let latestVersion: String?
    public let releaseURL: String?
    public let upgrade: UpgradeHint
    public let checkedAtMs: Int64?

    enum CodingKeys: String, CodingKey {
        case updateAvailable = "update_available"
        case currentVersion = "current_version"
        case latestVersion = "latest_version"
        case releaseURL = "release_url"
        case upgrade
        case checkedAtMs = "checked_at_ms"
    }
}

/// The `data` object inside an `Ok` response, internally tagged by `kind`.
/// Unknown kinds decode to `.unknown` so new daemon responses never crash us.
public enum ResponseData: Decodable, Sendable {
    case pong
    case daemonStatus(DaemonStatus)
    case playback(Playback)
    case devices([Device])
    case queue(Queue)
    case clientSeed(ClientSeed)
    case searchResults([MediaItem])
    case searchStarted(query: String, version: UInt64)
    case playlists([Playlist])
    case mediaItems([MediaItem])
    /// A page of liked songs plus the library `total` and page `offset`, so the
    /// UI can size the full list and lazy-load more as the user scrolls.
    case savedTracksPage(items: [MediaItem], total: Int, offset: Int)
    case listenSessions([ListenSession])
    case lyrics(SyncedLyrics?, offsetMs: Int64)
    case coverArt(path: String, cacheHit: Bool, bytes: UInt64, fetchedAtMs: Int64?)
    case mutation(CommandReceipt)
    case ack(message: String)
    case webApiToken(String?)
    case reminders([Reminder])
    case notifications([ReminderNotification])
    case reminderCreated(Reminder)
    case updateStatus(UpdateStatus)
    case unknown(kind: String)

    private enum CodingKeys: String, CodingKey {
        case kind, playback, devices, queue, items, query, version
        case playlists, lyrics, status, total, offset
        case offsetMs = "offset_ms"
        case path, bytes
        case cacheHit = "cache_hit"
        case fetchedAtMs = "fetched_at_ms"
        case receipt, message, token
        case reminders, notifications, reminder, sessions
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "pong":
            self = .pong
        case "daemon-status":
            self = .daemonStatus(try c.decode(DaemonStatus.self, forKey: .status))
        case "playback":
            self = .playback(try c.decode(Playback.self, forKey: .playback))
        case "devices":
            self = .devices(try c.decode([Device].self, forKey: .devices))
        case "queue":
            self = .queue(try c.decode(Queue.self, forKey: .queue))
        case "client-seed":
            self = .clientSeed(try ClientSeed(from: decoder))
        case "search-results":
            self = .searchResults(try c.decode([MediaItem].self, forKey: .items))
        case "search-started":
            self = .searchStarted(
                query: try c.decode(String.self, forKey: .query),
                version: try c.decode(UInt64.self, forKey: .version))
        case "playlists":
            self = .playlists(try c.decode([Playlist].self, forKey: .playlists))
        case "media-items":
            self = .mediaItems(try c.decode([MediaItem].self, forKey: .items))
        case "saved-tracks-page":
            self = .savedTracksPage(
                items: try c.decode([MediaItem].self, forKey: .items),
                total: try c.decode(Int.self, forKey: .total),
                offset: try c.decode(Int.self, forKey: .offset))
        case "listen-sessions":
            self = .listenSessions(try c.decode([ListenSession].self, forKey: .sessions))
        case "lyrics":
            self = .lyrics(
                try c.decodeIfPresent(SyncedLyrics.self, forKey: .lyrics),
                offsetMs: try c.decodeIfPresent(Int64.self, forKey: .offsetMs) ?? 0)
        case "cover-art":
            self = .coverArt(
                path: try c.decode(String.self, forKey: .path),
                cacheHit: try c.decode(Bool.self, forKey: .cacheHit),
                bytes: try c.decode(UInt64.self, forKey: .bytes),
                fetchedAtMs: try c.decodeIfPresent(Int64.self, forKey: .fetchedAtMs))
        case "mutation":
            self = .mutation(try c.decode(CommandReceipt.self, forKey: .receipt))
        case "ack":
            self = .ack(message: try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        case "web-api-token":
            self = .webApiToken(try c.decodeIfPresent(String.self, forKey: .token))
        case "reminders":
            self = .reminders(try c.decode([Reminder].self, forKey: .reminders))
        case "notifications":
            self = .notifications(try c.decode([ReminderNotification].self, forKey: .notifications))
        case "reminder-created":
            self = .reminderCreated(try c.decode(Reminder.self, forKey: .reminder))
        case "update-status":
            self = .updateStatus(try UpdateStatus(from: decoder))
        default:
            self = .unknown(kind: kind)
        }
    }
}
