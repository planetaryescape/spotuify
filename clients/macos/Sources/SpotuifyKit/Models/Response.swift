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

/// The `data` object inside an `Ok` response, internally tagged by `kind`.
/// Unknown kinds decode to `.unknown` so new daemon responses never crash us.
public enum ResponseData: Decodable, Sendable {
    case pong
    case playback(Playback)
    case devices([Device])
    case queue(Queue)
    case clientSeed(ClientSeed)
    case searchResults([MediaItem])
    case searchStarted(query: String, version: UInt64)
    case playlists([Playlist])
    case mediaItems([MediaItem])
    case lyrics(SyncedLyrics?, offsetMs: Int64)
    case mutation(CommandReceipt)
    case ack(message: String)
    case webApiToken(String?)
    case unknown(kind: String)

    private enum CodingKeys: String, CodingKey {
        case kind, playback, devices, queue, items, query, version
        case playlists, lyrics
        case offsetMs = "offset_ms"
        case receipt, message, token
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "pong":
            self = .pong
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
        case "lyrics":
            self = .lyrics(
                try c.decodeIfPresent(SyncedLyrics.self, forKey: .lyrics),
                offsetMs: try c.decodeIfPresent(Int64.self, forKey: .offsetMs) ?? 0)
        case "mutation":
            self = .mutation(try c.decode(CommandReceipt.self, forKey: .receipt))
        case "ack":
            self = .ack(message: try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        case "web-api-token":
            self = .webApiToken(try c.decodeIfPresent(String.self, forKey: .token))
        default:
            self = .unknown(kind: kind)
        }
    }
}
