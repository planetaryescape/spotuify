import Foundation
import AppKit
import MediaPlayer
import SpotuifyKit

private struct NowPlayingArtworkPayload: @unchecked Sendable {
    let boundsSize: CGSize
    let tiffData: Data

    func image() -> NSImage {
        NSImage(data: tiffData) ?? NSImage(size: boundsSize)
    }
}

/// MediaPlayer invokes artwork request handlers on its own queue. Keep this
/// helper outside the main-actor controller so the closure is not actor-bound.
private func makeNowPlayingArtwork(from image: NSImage) -> MPMediaItemArtwork? {
    guard let tiffData = image.tiffRepresentation else { return nil }
    let payload = NowPlayingArtworkPayload(boundsSize: image.size, tiffData: tiffData)
    return MPMediaItemArtwork(boundsSize: payload.boundsSize) { _ in payload.image() }
}

/// Bridges the player to macOS: publishes Now Playing info (Control Center,
/// lock screen) and routes hardware media keys / remote commands back to the
/// daemon. Commands never mutate local state — they send to the daemon and the
/// resulting event updates everything (daemon-owned state).
///
/// Elapsed time is anchored only on real playback changes; macOS extrapolates
/// from `playbackRate`, so the 250 ms in-app ticker need not push here.
@MainActor
final class SystemMediaController {
    static let shared = SystemMediaController()

    private weak var model: AppModel?
    private var configured = false
    private var cachedArtworkURL: String?
    private var cachedImage: NSImage?

    private init() {}

    /// Register remote-command handlers once.
    func configure(model: AppModel) {
        self.model = model
        guard !configured else { return }
        configured = true

        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in self?.model?.send(.resume); return .success }
        center.pauseCommand.addTarget { [weak self] _ in self?.model?.send(.pause); return .success }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in self?.model?.togglePlayPause(); return .success }
        center.nextTrackCommand.addTarget { [weak self] _ in self?.model?.next(); return .success }
        center.previousTrackCommand.addTarget { [weak self] _ in self?.model?.previous(); return .success }
        center.changePlaybackPositionCommand.isEnabled = true
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self,
                  let positionEvent = event as? MPChangePlaybackPositionCommandEvent,
                  let duration = self.model?.player.durationMs, duration > 0 else {
                return .commandFailed
            }
            self.model?.seek(toFraction: positionEvent.positionTime * 1000 / Double(duration))
            return .success
        }
    }

    /// Publish the current track/state to the Now Playing center.
    func updateNowPlaying(player: PlayerStore) async {
        let center = MPNowPlayingInfoCenter.default()
        guard let item = player.currentItem else {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            return
        }

        var info: [String: Any] = [
            MPMediaItemPropertyTitle: item.name,
            MPMediaItemPropertyArtist: item.subtitle,
            MPMediaItemPropertyPlaybackDuration: Double(item.durationMs) / 1000.0,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: Double(player.displayProgressMs) / 1000.0,
            MPNowPlayingInfoPropertyPlaybackRate: player.isPlaying ? 1.0 : 0.0,
        ]
        if item.context.isEmpty == false {
            info[MPMediaItemPropertyAlbumTitle] = item.context
        }
        if let image = await artworkImage(for: item.imageURL),
           let artwork = makeNowPlayingArtwork(from: image) {
            info[MPMediaItemPropertyArtwork] = artwork
        }

        center.nowPlayingInfo = info
        center.playbackState = player.isPlaying ? .playing : .paused
    }

    private func artworkImage(for urlString: String?) async -> NSImage? {
        guard let urlString else { return nil }
        if urlString == cachedArtworkURL, let cachedImage { return cachedImage }
        guard let image = await CoverArtCache.shared.image(for: urlString) else { return nil }
        cachedArtworkURL = urlString
        cachedImage = image
        return image
    }
}
