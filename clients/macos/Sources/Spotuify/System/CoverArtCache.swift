import AppKit
import SpotuifyKit

/// Loads and caches album artwork straight from the Spotify CDN URLs the
/// daemon hands us (`MediaItem.image_url`). MainActor-confined so `NSImage`
/// never crosses an isolation boundary; network fetches still happen async.
@MainActor
final class CoverArtCache {
    static let shared = CoverArtCache()

    private let cache = NSCache<NSString, NSImage>()
    private var inFlight: [String: Task<NSImage?, Never>] = [:]

    private init() {
        cache.countLimit = 200
    }

    func image(for urlString: String?) async -> NSImage? {
        guard let urlString, !urlString.isEmpty else { return nil }
        if let cached = cache.object(forKey: urlString as NSString) { return cached }
        if let existing = inFlight[urlString] { return await existing.value }

        let task = Task { () -> NSImage? in
            guard let url = URL(string: urlString) else { return nil }
            guard let (data, _) = try? await URLSession.shared.data(from: url) else { return nil }
            return NSImage(data: data)
        }
        inFlight[urlString] = task
        let image = await task.value
        inFlight[urlString] = nil
        if let image { cache.setObject(image, forKey: urlString as NSString) }
        return image
    }
}
