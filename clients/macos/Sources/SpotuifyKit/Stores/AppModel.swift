import Foundation
import Observation
import os

/// Top-level coordinator: owns the daemon connection and feature stores, runs
/// the event pump, and supervises connect → subscribe → seed with exponential
/// backoff reconnection. The single mutation path to the daemon is through
/// here; views call its command helpers and never mutate store state.
@MainActor
@Observable
public final class AppModel {
    public let player = PlayerStore()
    public let search = SearchStore()
    public let library = LibraryStore()
    public let lyrics = LyricsStore()
    public let viz = VizStore()
    public private(set) var connectionState: ConnectionState = .idle
    public private(set) var recent: [MediaItem] = []
    /// Transient status line (rate-limit countdown, premium/auth notice, …).
    public private(set) var banner: String?

    private let connection = DaemonConnection()
    private let logger = Logger(subsystem: "com.bhekanik.spotuify", category: "appmodel")
    private let debugEnabled = ProcessInfo.processInfo.environment["SPOTUIFY_DEBUG_LOG"] == "1"
    private var started = false
    private var eventTask: Task<Void, Never>?
    private var supervisor: Task<Void, Never>?

    /// Extra event observers (feature stores subscribe here).
    private var eventObservers: [(DaemonEvent) -> Void] = []

    public init() {
        search.connect(self)
        library.connect(self)
        lyrics.connect(self)
    }

    public var isReady: Bool { connectionState == .ready }

    /// Idempotent — safe to call from both the window and menubar scenes.
    public func start() {
        guard !started else { return }
        started = true
        eventTask = Task { [weak self] in
            guard let self else { return }
            for await event in self.connection.events {
                self.handle(event)
            }
        }
        supervisor = Task { [weak self] in await self?.runSupervisor() }
    }

    public func addEventObserver(_ observer: @escaping (DaemonEvent) -> Void) {
        eventObservers.append(observer)
    }

    /// Issue a request through the shared connection (used by feature stores).
    @discardableResult
    public func request(_ request: DaemonRequest, timeout: Duration = .seconds(30)) async throws -> ResponseData {
        try await connection.request(request, timeout: timeout)
    }

    // MARK: Command helpers (fire-and-forget; state arrives via events)

    public func send(_ command: PlaybackCommand) {
        Task { [weak self] in try? await self?.connection.request(.playbackCommand(command)) }
    }

    public func togglePlayPause() { send(.toggle) }
    public func next() { send(.next) }
    public func previous() { send(.previous) }
    public func play(uri: String) { send(.playURI(uri)) }
    public func toggleShuffle() { send(.shuffle(!player.shuffle)) }

    public func seek(toFraction fraction: Double) {
        let duration = player.durationMs
        guard duration > 0 else { return }
        let clamped = max(0, min(1, fraction))
        send(.seek(positionMs: UInt64(Double(duration) * clamped)))
    }

    public func seek(toMs ms: UInt64) {
        send(.seek(positionMs: ms))
    }

    public func setVolume(_ percent: Int) {
        send(.volume(percent: UInt8(max(0, min(100, percent)))))
    }

    public func cycleRepeat() {
        let next: RepeatMode
        switch player.repeatMode {
        case .off: next = .context
        case .context: next = .track
        case .track: next = .off
        }
        send(.repeatMode(next))
    }

    public func transfer(to device: Device) {
        let target = device.deviceID ?? device.name
        Task { [weak self] in try? await self?.connection.request(.deviceTransfer(device: target)) }
    }

    public func queueAdd(uri: String) {
        Task { [weak self] in try? await self?.connection.request(.queueAdd(uri: uri)) }
    }

    /// Append many tracks in one request (e.g. "queue all liked songs").
    public func queueAll(uris: [String]) {
        guard !uris.isEmpty else { return }
        Task { [weak self] in try? await self?.connection.request(.queueAddMany(uris: uris)) }
    }

    /// Play a list with no single context URI (e.g. Liked Songs): start the
    /// first track, then queue the rest.
    public func playAll(uris: [String]) {
        guard let first = uris.first else { return }
        Task { [weak self] in
            guard let self else { return }
            try? await self.connection.request(.playbackCommand(.playURI(first)))
            let rest = Array(uris.dropFirst())
            if !rest.isEmpty {
                try? await self.connection.request(.queueAddMany(uris: rest))
            }
        }
    }

    public func shufflePlay(uris: [String]) {
        playAll(uris: uris.shuffled())
    }

    public func clearBanner() { banner = nil }

    /// The daemon socket this client targets (for display/diagnostics).
    public var socketPath: String { SocketPath.resolve() }

    /// Drop the current connection; the supervisor reconnects + re-seeds.
    public func forceReconnect() {
        Task { await connection.close() }
    }

    // MARK: Event handling

    func handle(_ event: DaemonEvent) {
        switch event {
        case .playbackChanged(_, let playback):
            if let playback { player.applyPlayback(playback) }
            else { Task { await refresh(.playback) } }
        case .queueChanged(_, _, let queue):
            if let queue { player.applyQueue(queue) }
            else { Task { await refresh(.queue) } }
        case .devicesChanged(_, let devices):
            if let devices { player.applyDevices(devices) }
            else { Task { await refresh(.devices) } }
        case .spectrumFrame(let bands, let peak, _):
            viz.apply(bands: bands, peak: peak)
        case .eventStreamLagged:
            Task { try? await reseed() }
        case .rateLimited(let secs, _):
            banner = "Rate limited — retrying in \(secs)s"
        case .premiumRequired:
            banner = "Spotify Premium required for playback"
        case .authError:
            banner = "Sign-in needed — run `spotuify login`"
        case .playerFailed(let reason, _):
            banner = "Player failed: \(reason). Run `spotuify reconnect`."
        case .playerReady:
            banner = nil
        default:
            break
        }
        debugLog("event \(event)")
        for observer in eventObservers { observer(event) }
    }

    // MARK: Supervisor

    private func runSupervisor() async {
        let path = SocketPath.resolve()
        var attempt = 0
        while !Task.isCancelled {
            connectionState = attempt == 0 ? .connecting : .reconnecting(attempt: attempt)
            debugLog("connecting attempt=\(attempt) path=\(path)")
            _ = await DaemonLauncher.ensureRunning(socketPath: path)
            do {
                try await connection.connect(to: path)
                try await connection.subscribeEvents()
                try await reseed()
                connectionState = .ready
                banner = nil
                attempt = 0
                debugLog("ready")
                await connection.waitUntilClosed()
                debugLog("disconnected")
            } catch {
                connectionState = .failed("\(error)")
                debugLog("connect failed: \(error)")
            }
            guard !Task.isCancelled else { break }
            attempt += 1
            let delayMs = min(10_000, 250 * (1 << min(attempt, 6)))
            try? await Task.sleep(for: .milliseconds(delayMs))
        }
    }

    private func reseed() async throws {
        if case .clientSeed(let seed) = try await connection.request(.clientSeed, timeout: .seconds(10)) {
            player.applyPlayback(seed.playback)
            player.applyQueue(seed.queue)
            player.applyDevices(seed.devices)
            recent = seed.recent
            debugLog("seeded devices=\(seed.devices.count) recent=\(seed.recent.count) "
                + "track=\(seed.playback.item?.name ?? "<none>")")
        }
    }

    private enum RefreshKind { case playback, queue, devices }

    private func refresh(_ kind: RefreshKind) async {
        do {
            switch kind {
            case .playback:
                if case .playback(let playback) = try await connection.request(.playbackGet) {
                    player.applyPlayback(playback)
                }
            case .queue:
                if case .queue(let queue) = try await connection.request(.queueGet) {
                    player.applyQueue(queue)
                }
            case .devices:
                if case .devices(let devices) = try await connection.request(.devicesList) {
                    player.applyDevices(devices)
                }
            }
        } catch {
            debugLog("refresh \(kind) failed: \(error)")
        }
    }

    private func debugLog(_ message: String) {
        logger.notice("\(message, privacy: .public)")
        if debugEnabled, let line = "[spotuify] \(message)\n".data(using: .utf8) {
            FileHandle.standardError.write(line) // stderr is unbuffered
        }
    }
}
