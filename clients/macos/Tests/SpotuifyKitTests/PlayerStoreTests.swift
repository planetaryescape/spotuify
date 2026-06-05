import Foundation
import Testing
@testable import SpotuifyKit

@MainActor
@Suite("PlayerStore")
struct PlayerStoreTests {
    private func track(durationMs: UInt64 = 200_000) -> MediaItem {
        MediaItem(
            spotifyID: "t1", uri: "spotify:track:t1", name: "Track", subtitle: "Artist",
            context: "", durationMs: durationMs, imageURL: nil, kind: .track,
            source: nil, freshness: nil, explicit: false, isPlayable: true)
    }

    @Test("paused snapshot shows exact progress, no interpolation")
    func pausedProgress() {
        let store = PlayerStore()
        let playback = Playback(
            item: track(), device: nil, isPlaying: false, progressMs: 42_000,
            shuffle: false, repeatMode: "off", sampledAtMs: 1, providerTimestampMs: nil, source: "cache")
        store.applyPlayback(playback)
        #expect(store.displayProgressMs == 42_000)
        #expect(store.isPlaying == false)
        #expect(store.durationMs == 200_000)
    }

    @Test("playing snapshot interpolates forward from sampled_at_ms")
    func playingInterpolation() {
        let store = PlayerStore()
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let playback = Playback(
            item: track(), device: nil, isPlaying: true, progressMs: 10_000,
            shuffle: false, repeatMode: "off",
            sampledAtMs: nowMs - 3_000, // sampled 3s ago
            providerTimestampMs: nil, source: "player-event")
        store.applyPlayback(playback)
        // Should have advanced ~3s past the 10s sample, never beyond duration.
        #expect(store.displayProgressMs >= 12_500)
        #expect(store.displayProgressMs <= store.durationMs)
    }

    @Test("interpolation clamps to track duration")
    func clampsToDuration() {
        let store = PlayerStore()
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let playback = Playback(
            item: track(durationMs: 5_000), device: nil, isPlaying: true, progressMs: 4_000,
            shuffle: false, repeatMode: "off",
            sampledAtMs: nowMs - 60_000, // way past the end
            providerTimestampMs: nil, source: "player-event")
        store.applyPlayback(playback)
        #expect(store.displayProgressMs == 5_000)
        #expect(store.progressFraction == 1.0)
    }

    @Test("repeat mode and active device derive correctly")
    func derivedState() {
        let store = PlayerStore()
        let device = Device(
            deviceID: "d1", name: "spotuify-hume", kind: "computer",
            isActive: true, isRestricted: false, volumePercent: 64, supportsVolume: true)
        store.applyDevices([device])
        let playback = Playback(
            item: track(), device: device, isPlaying: true, progressMs: 0,
            shuffle: true, repeatMode: "track", sampledAtMs: nil, providerTimestampMs: nil, source: nil)
        store.applyPlayback(playback)
        #expect(store.shuffle == true)
        #expect(store.repeatMode == .track)
        #expect(store.activeDevice?.name == "spotuify-hume")
        #expect(store.volumePercent == 64)
    }
}

@MainActor
@Suite("AppModel event routing")
struct AppModelEventTests {
    @Test("playback-changed with embedded snapshot updates the player store")
    func playbackEventUpdatesStore() {
        let model = AppModel()
        let playback = Playback(
            item: MediaItem(
                spotifyID: nil, uri: "spotify:track:x", name: "Embedded", subtitle: "Artist",
                context: "", durationMs: 100_000, imageURL: nil, kind: .track,
                source: nil, freshness: nil, explicit: nil, isPlayable: nil),
            device: nil, isPlaying: true, progressMs: 1_000,
            shuffle: false, repeatMode: "off", sampledAtMs: nil, providerTimestampMs: nil, source: "player-event")
        model.handle(.playbackChanged(action: "optimistic-resume", playback: playback))
        #expect(model.player.currentItem?.name == "Embedded")
        #expect(model.player.isPlaying)
    }

    @Test("rate-limited event surfaces a banner")
    func rateLimitBanner() {
        let model = AppModel()
        model.handle(.rateLimited(retryAfterSecs: 5, scope: "search"))
        #expect(model.banner?.contains("5s") == true)
    }
}
