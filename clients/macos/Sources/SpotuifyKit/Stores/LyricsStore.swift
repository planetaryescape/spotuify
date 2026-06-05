import Foundation
import Observation

/// Fetches synced lyrics for a track on demand (driven by the Lyrics view's
/// visibility) and resolves the active line from playback position + offset.
@MainActor
@Observable
public final class LyricsStore {
    public private(set) var lyrics: SyncedLyrics?
    public private(set) var offsetMs: Int64 = 0
    public private(set) var loading = false
    public private(set) var loadedURI: String?

    private weak var model: AppModel?

    public init() {}

    func connect(_ model: AppModel) { self.model = model }

    public func load(uri: String?) async {
        guard let uri else { lyrics = nil; loadedURI = nil; return }
        guard uri != loadedURI, let model else { return }
        loadedURI = uri
        loading = true
        lyrics = nil
        defer { loading = false }
        if case .lyrics(let synced, let offset) = try? await model.request(
            .lyricsGet(trackURI: uri, forceRefresh: false), timeout: .seconds(20)) {
            lyrics = synced
            offsetMs = offset
        }
    }

    public func activeIndex(positionMs: UInt64) -> Int? {
        lyrics?.activeLineIndex(positionMs: positionMs, offsetMs: offsetMs)
    }
}
