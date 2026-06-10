import AppKit
import SpotuifyKit

/// Loads album artwork through the daemon's disk cache, with an in-process
/// `NSImage` cache for already-decoded images.
@MainActor
final class CoverArtCache {
    static let shared = CoverArtCache()

    private let daemon = DaemonConnection()
    private let cache = NSCache<NSString, NSImage>()
    private var inFlight: [String: Task<NSImage?, Never>] = [:]
    private var daemonConnected = false

    private init() {
        cache.countLimit = 200
    }

    func image(for urlString: String?) async -> NSImage? {
        guard let urlString, !urlString.isEmpty else { return nil }
        if let cached = cache.object(forKey: urlString as NSString) { return cached }
        if let existing = inFlight[urlString] { return await existing.value }

        let task = Task { () -> NSImage? in
            if let cached = await self.imageFromDaemonCache(urlString) {
                return cached
            }
            return await self.imageFromNetwork(urlString)
        }
        inFlight[urlString] = task
        let image = await task.value
        inFlight[urlString] = nil
        if let image { cache.setObject(image, forKey: urlString as NSString) }
        return image
    }

    private func imageFromDaemonCache(_ urlString: String) async -> NSImage? {
        guard await ensureDaemonConnected() else { return nil }
        do {
            let response = try await daemon.request(.coverArt(url: urlString), timeout: .seconds(10))
            guard case .coverArt(let path, _, _, _) = response else { return nil }
            return NSImage(contentsOfFile: path)
        } catch {
            daemonConnected = false
            return nil
        }
    }

    private func ensureDaemonConnected() async -> Bool {
        if daemonConnected { return true }
        do {
            try await daemon.connect(to: SocketPath.resolve())
            daemonConnected = true
            return true
        } catch {
            daemonConnected = false
            return false
        }
    }

    private func imageFromNetwork(_ urlString: String) async -> NSImage? {
        guard let url = URL(string: urlString),
              let (data, _) = try? await URLSession.shared.data(from: url)
        else { return nil }
        return NSImage(data: data)
    }
}
