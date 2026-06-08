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
    /// Last track we successfully published. Daemon snapshots can arrive with a
    /// nil `item` (optimistic / state-only events) while audio keeps playing;
    /// retaining the last track stops Control Center from blanking out.
    private var lastItem: MediaItem?

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
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self,
                  let positionEvent = event as? MPChangePlaybackPositionCommandEvent,
                  let duration = self.model?.player.durationMs, duration > 0 else {
                return .commandFailed
            }
            self.model?.seek(toFraction: positionEvent.positionTime * 1000 / Double(duration))
            return .success
        }

        // Explicitly enable the commands we handle. A headphone/AirPods single
        // press maps to `togglePlayPauseCommand` (some devices send play/pause
        // separately), so all three must be live. Disable the ones we don't
        // implement so the system's command set is coherent and it doesn't
        // mis-route a button to an unhandled command.
        for command in [
            center.playCommand, center.pauseCommand, center.togglePlayPauseCommand,
            center.nextTrackCommand, center.previousTrackCommand,
            center.changePlaybackPositionCommand,
        ] {
            command.isEnabled = true
        }
        for command in [
            center.seekForwardCommand, center.seekBackwardCommand,
            center.skipForwardCommand, center.skipBackwardCommand,
            center.changeRepeatModeCommand, center.changeShuffleModeCommand,
        ] {
            command.isEnabled = false
        }

        // Claim Now Playing immediately. macOS only delivers media-key /
        // headphone events to the app that is the current Now Playing source,
        // which requires a published `nowPlayingInfo` + `playbackState`. Without
        // this initial publish the app stays a non-source until the *next*
        // playback change, so the first headphone press goes nowhere.
        Task { await self.updateNowPlaying(player: model.player) }
    }

    /// Publish the current track/state to the Now Playing center.
    func updateNowPlaying(player: PlayerStore) async {
        let center = MPNowPlayingInfoCenter.default()

        // The session is genuinely over only when the daemon reports no playback
        // and no current track — then clear everything.
        if player.playback == nil, player.currentItem == nil {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            lastItem = nil
            return
        }
        // Remember the most recent real track; fall back to it through transient
        // nil-item snapshots so Control Center never shows a blank entry while
        // audio is still playing.
        if let current = player.currentItem { lastItem = current }
        guard let item = player.currentItem ?? lastItem else {
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
