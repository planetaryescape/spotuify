import Foundation

/// `playback-speed` response: the podcast speed setting, the rate the current
/// item is actually playing at (music is always 1.0), and whether a local
/// player is honouring it (false on a remote Connect device).
public struct PlaybackSpeedInfo: Codable, Sendable, Equatable {
    public let speed: Double
    public let effective: Double
    public let applied: Bool

    /// The picker steps Spotify offers.
    public static let presets: [Double] = [0.5, 0.8, 1.0, 1.2, 1.5, 1.8, 2.0, 2.5, 3.0, 3.5]

    public static func label(_ speed: Double) -> String {
        speed == speed.rounded() ? "\(Int(speed))x" : String(format: "%.2gx", speed)
    }
}

/// A saved position inside a media item (mirrors `spotuify_core::Bookmark`).
public struct Bookmark: Codable, Sendable, Hashable, Identifiable {
    public let id: String
    public let mediaURI: String
    public let mediaKind: MediaKind
    public let name: String
    public let subtitle: String
    public let imageURL: String?
    public let positionMs: UInt64
    public let note: String?
    public let createdAtMs: Int64

    enum CodingKeys: String, CodingKey {
        case id
        case mediaURI = "media_uri"
        case mediaKind = "media_kind"
        case name, subtitle
        case imageURL = "image_url"
        case positionMs = "position_ms"
        case note
        case createdAtMs = "created_at_ms"
    }

    public var createdDate: Date { Date(timeIntervalSince1970: Double(createdAtMs) / 1000) }

    /// `h:mm:ss` past the hour, else `m:ss` — podcast positions routinely
    /// sit past 60 minutes.
    public var positionLabel: String {
        let total = Int(positionMs / 1000)
        let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}
