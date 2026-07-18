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
    public let provider: ProviderID?
    public let detail: String?

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

public enum AuthSessionState: Sendable, Equatable {
    case starting
    case awaitingUser(authorizationURL: String, redirectURI: String, browserError: String?)
    case waiting(authorizationURL: String, redirectURI: String, browserError: String?)
    case authorized
    case failed(message: String)
    case cancelled
    /// A state string this client build doesn't know. Falls back here instead
    /// of throwing so one unknown value never fails the whole auth response.
    case unknown(state: String)
}

extension AuthSessionState: Decodable {
    private enum CodingKeys: String, CodingKey {
        case state
        case authorizationURL = "authorization_url"
        case redirectURI = "redirect_uri"
        case browserError = "browser_error"
        case message
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let state = try c.decode(String.self, forKey: .state)
        switch state {
        case "starting": self = .starting
        case "awaiting_user":
            self = .awaitingUser(
                authorizationURL: try c.decode(String.self, forKey: .authorizationURL),
                redirectURI: try c.decode(String.self, forKey: .redirectURI),
                browserError: try c.decodeIfPresent(String.self, forKey: .browserError))
        case "waiting":
            self = .waiting(
                authorizationURL: try c.decode(String.self, forKey: .authorizationURL),
                redirectURI: try c.decode(String.self, forKey: .redirectURI),
                browserError: try c.decodeIfPresent(String.self, forKey: .browserError))
        case "authorized": self = .authorized
        case "failed": self = .failed(message: try c.decode(String.self, forKey: .message))
        case "cancelled": self = .cancelled
        default: self = .unknown(state: state)
        }
    }
}

public struct AuthSession: Decodable, Sendable, Equatable {
    public let sessionID: UUID
    public let provider: ProviderID
    public let method: String
    public let state: AuthSessionState
    public let createdAtMs: Int64
    public let expiresAtMs: Int64

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case provider, method, state
        case createdAtMs = "created_at_ms"
        case expiresAtMs = "expires_at_ms"
    }
}

public enum AuthStrategy: String, Decodable, Sendable, Equatable {
    case none
    case spotifyOauth = "spotify_oauth"
    /// An unrecognized strategy from a newer daemon; decodes here instead of
    /// throwing so one unknown value never fails the whole auth response.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = AuthStrategy(rawValue: raw) ?? .unknown
    }
}

public enum AuthCredentialKind: String, Decodable, Sendable, Equatable {
    case devApp = "dev_app"
    case firstParty = "first_party"
    /// An unrecognized credential kind from a newer daemon; decodes here
    /// instead of throwing so one unknown value never fails the response.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = AuthCredentialKind(rawValue: raw) ?? .unknown
    }
}

public struct AuthCredentialStatus: Decodable, Sendable {
    public let kind: AuthCredentialKind
    public let present: Bool
    public let expiresAtMs: Int64?
    public let scopes: [String]
    public let missingScopes: [String]

    enum CodingKeys: String, CodingKey {
        case kind, present, scopes
        case expiresAtMs = "expires_at_ms"
        case missingScopes = "missing_scopes"
    }
}

public struct AuthStatus: Decodable, Sendable {
    public let provider: ProviderID
    public let strategy: AuthStrategy
    public let authRequired: Bool
    public let authRevoked: Bool
    public let credentials: [AuthCredentialStatus]

    enum CodingKeys: String, CodingKey {
        case provider, strategy, credentials
        case authRequired = "auth_required"
        case authRevoked = "auth_revoked"
    }
}

public struct AuthLogout: Decodable, Sendable {
    public let provider: ProviderID
    public let removedDevApp: Bool
    public let removedFirstParty: Bool
    public let removedLibrespot: Bool
    public let authRequired: Bool

    enum CodingKeys: String, CodingKey {
        case provider
        case removedDevApp = "removed_dev_app"
        case removedFirstParty = "removed_first_party"
        case removedLibrespot = "removed_librespot"
        case authRequired = "auth_required"
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
    case providerList(defaultProvider: ProviderID?, providers: [ProviderDescriptor])
    case targetResolved(ResolvedTarget?)
    case audioOutputs(outputs: [String], selected: String?)
    case searchResults([MediaItem])
    case searchStarted(query: String, version: UInt64, provider: ProviderID?)
    case sync(CacheSyncSummary)
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
    case authSession(AuthSession)
    case authStatus(AuthStatus)
    case authLogout(AuthLogout)
    case webApiToken(String?)
    case reminders([Reminder])
    case notifications([ReminderNotification])
    case reminderCreated(Reminder)
    case updateStatus(UpdateStatus)
    case unknown(kind: String)

    private enum CodingKeys: String, CodingKey {
        case kind, playback, devices, queue, items, query, version, provider
        case defaultProvider = "default_provider"
        case providers, target, outputs, selected, summary
        case playlists, lyrics, status, total, offset
        case offsetMs = "offset_ms"
        case path, bytes
        case cacheHit = "cache_hit"
        case fetchedAtMs = "fetched_at_ms"
        case receipt, message, token, session, result
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
        case "provider-list":
            self = .providerList(
                defaultProvider: try c.decodeIfPresent(ProviderID.self, forKey: .defaultProvider),
                providers: try c.decode([ProviderDescriptor].self, forKey: .providers))
        case "target-resolved":
            self = .targetResolved(try c.decodeIfPresent(ResolvedTarget.self, forKey: .target))
        case "audio-outputs":
            self = .audioOutputs(
                outputs: try c.decode([String].self, forKey: .outputs),
                selected: try c.decodeIfPresent(String.self, forKey: .selected))
        case "search-results":
            self = .searchResults(try c.decode([MediaItem].self, forKey: .items))
        case "search-started":
            self = .searchStarted(
                query: try c.decode(String.self, forKey: .query),
                version: try c.decode(UInt64.self, forKey: .version),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "sync":
            self = .sync(try c.decode(CacheSyncSummary.self, forKey: .summary))
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
        case "auth-session":
            self = .authSession(try c.decode(AuthSession.self, forKey: .session))
        case "auth-status":
            self = .authStatus(try c.decode(AuthStatus.self, forKey: .status))
        case "auth-logout":
            self = .authLogout(try c.decode(AuthLogout.self, forKey: .result))
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
