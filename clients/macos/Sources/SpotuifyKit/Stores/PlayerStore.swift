import Foundation
import Observation

/// Daemon-authoritative playback state for the UI. The store is event-sourced:
/// it is only ever updated by applying daemon snapshots/events — never by the
/// UI directly. Between discrete `playback-changed` events a local ticker
/// interpolates the displayed progress from `progress_ms` + `sampled_at_ms`,
/// then snaps back to truth on the next event.
@MainActor
@Observable
public final class PlayerStore {
    public private(set) var playback: Playback?
    public private(set) var queue: Queue?
    public private(set) var devices: [Device] = []
    public private(set) var displayProgressMs: UInt64 = 0

    private var ticker: Task<Void, Never>?

    public init() {}

    // MARK: Apply daemon state

    public func applyPlayback(_ playback: Playback?) {
        self.playback = playback
        recomputeProgress()
        restartTicker()
    }

    public func applyQueue(_ queue: Queue?) {
        self.queue = queue
    }

    public func applyDevices(_ devices: [Device]) {
        self.devices = devices
    }

    // MARK: Derived view state

    /// The track to display. The daemon sometimes reports `playback.item` as
    /// nil while the queue still knows what's loaded, so fall back to that.
    public var currentItem: MediaItem? { playback?.item ?? queue?.currentlyPlaying }
    public var isPlaying: Bool { playback?.isPlaying ?? false }
    public var durationMs: UInt64 { currentItem?.durationMs ?? 0 }
    public var shuffle: Bool { playback?.shuffle ?? false }
    public var repeatMode: RepeatMode { RepeatMode(rawValue: playback?.repeatMode ?? "off") ?? .off }

    /// The device playback is on, falling back to the one flagged active.
    public var activeDevice: Device? {
        playback?.device ?? devices.first { $0.isActive }
    }

    public var volumePercent: UInt8? { activeDevice?.volumePercent }

    /// 0...1 progress for the seek bar.
    public var progressFraction: Double {
        let duration = durationMs
        guard duration > 0 else { return 0 }
        return min(1, Double(displayProgressMs) / Double(duration))
    }

    // MARK: Progress interpolation

    private func recomputeProgress() {
        guard let playback else { displayProgressMs = 0; return }
        guard playback.isPlaying, let sampledAt = playback.sampledAtMs else {
            displayProgressMs = playback.progressMs
            return
        }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let elapsed = max(0, nowMs - sampledAt)
        let projected = Int64(playback.progressMs) + elapsed
        let duration = currentItem?.durationMs ?? UInt64(Int64.max)
        displayProgressMs = UInt64(max(0, min(projected, Int64(duration))))
    }

    private func restartTicker() {
        ticker?.cancel()
        guard playback?.isPlaying == true else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(250))
                guard let self else { return }
                self.recomputeProgress()
            }
        }
    }
}
