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

/// `eq` response: the persisted 10-band curve, whether a local player is
/// filtering with it right now (false on a remote Connect device), and the
/// gain reduction its peak limiter is applying.
public struct EqInfo: Codable, Sendable, Equatable {
    public let settings: EqSettings
    public let applied: Bool
    /// Negative dB while the limiter is working, 0 when idle. Decoded
    /// tolerantly so a daemon older than D036 still yields an `EqInfo`.
    public let limitingDB: Double

    enum CodingKeys: String, CodingKey {
        case settings
        case applied
        case limitingDB = "limiting_db"
    }

    public init(settings: EqSettings, applied: Bool, limitingDB: Double = 0) {
        self.settings = settings
        self.applied = applied
        self.limitingDB = limitingDB
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        settings = try container.decode(EqSettings.self, forKey: .settings)
        applied = try container.decode(Bool.self, forKey: .applied)
        limitingDB = try container.decodeIfPresent(Double.self, forKey: .limitingDB) ?? 0
    }

    /// `-2.4 dB` while limiting, `idle` otherwise — the same wording the CLI
    /// table and the TUI overlay use.
    public var limitingLabel: String {
        limitingDB < 0 ? String(format: "%.1f dB", limitingDB) : "idle"
    }
}

/// A 10-band EQ curve (mirrors `spotuify_core::EqSettings`). `preset` is nil
/// for a hand-edited curve, which the UI labels "Custom".
public struct EqSettings: Codable, Sendable, Equatable {
    public let preset: String?
    public let bands: [Double]

    public var label: String { preset ?? "Custom" }
    public var isFlat: Bool { bands.allSatisfy { $0 == 0 } }

    /// Preset names in daemon order. Kept in sync with
    /// `spotuify_core::EQ_PRESETS`; the daemon rejects anything else.
    public static let presets: [String] = [
        "Flat", "Rock", "Pop", "Jazz", "Classical", "Bass Boost", "Treble Boost",
        "Vocal", "Electronic", "Acoustic", "Hip-Hop", "R&B", "Loudness",
        "Late Night", "Podcast", "Small Speakers",
    ]

    public static let flat = EqSettings(preset: "Flat", bands: Array(repeating: 0, count: 10))
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
