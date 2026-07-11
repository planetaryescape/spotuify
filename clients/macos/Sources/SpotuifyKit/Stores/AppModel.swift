import Foundation
import Observation
import os

/// A newer release the daemon has observed, with how to upgrade this install.
public struct AvailableUpdate: Sendable, Equatable {
    public let latestVersion: String
    /// Shell command to upgrade (Homebrew/cargo), when applicable.
    public let command: String?
    /// URL to open (release page / DMG), when applicable.
    public let url: String?
}

/// Top-level coordinator: owns the daemon connection and feature stores, runs
/// the event pump, and supervises connect → subscribe → seed with exponential
/// backoff reconnection. The single mutation path to the daemon is through
/// here; views call its command helpers and never mutate store state.
@MainActor
@Observable
public final class AppModel {
    /// Sentinel context URI for the Liked Songs collection. Mirrors the
    /// daemon's `spotuify_protocol::LIKED_SONGS_CONTEXT`; sending it as a
    /// `PlayURI` context tells the daemon to play the whole Liked Songs
    /// list starting at the tapped track.
    public static let likedContext = "spotuify:collection:liked"

    public let player = PlayerStore()
    public let search = SearchStore()
    public let podcasts = PodcastsStore()
    public let config = ConfigStore()
    public let library = LibraryStore()
    public let lyrics = LyricsStore()
    public let reminders = RemindersStore()
    public let viz = VizStore()
    /// One-click in-app updater (download DMG → verify → swap bundle).
    public let updater = AppUpdater()
    /// Set true once per launch when due notifications exist on connect, so the
    /// shell can present the due-inbox modal exactly once.
    public var presentDueInbox = false
    private var dueInboxShown = false
    public private(set) var connectionState: ConnectionState = .idle
    public private(set) var readiness: DaemonReadiness = .checking
    public private(set) var recent: [MediaItem] = []
    /// Transient status line (rate-limit countdown, premium/auth notice, …).
    public private(set) var banner: String?
    /// A newer release is available (from the daemon's update check). Drives the
    /// update banner with a Download / upgrade-command action.
    public private(set) var availableUpdate: AvailableUpdate?
    /// Transient confirmation toast (e.g. "Added to queue"), auto-dismissed.
    /// Gives instant feedback for fire-and-forget mutations.
    public private(set) var toast: String?
    private var toastTask: Task<Void, Never>?

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
        podcasts.connect(self)
        config.connect(self)
        library.connect(self)
        lyrics.connect(self)
        reminders.connect(self)
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

    public func togglePlayPause() { send(player.isPlaying ? .pause : .resume) }
    public func next() { send(.next) }
    public func previous() { send(.previous) }
    public func play(uri: String) { send(.playURI(uri, contextURI: nil)) }

    /// Play `uri` inside a collection `contextURI` (album/playlist URI, or
    /// ``AppModel/likedContext`` for Liked Songs) so the daemon starts the
    /// whole collection at the tapped track and "Next" advances through it.
    /// Passing `nil` is identical to ``play(uri:)``.
    public func play(uri: String, contextURI: String?) {
        send(.playURI(uri, contextURI: contextURI))
    }

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

    /// Report this app's focus to the daemon. Viz focus is a per-client
    /// vote: the daemon broadcasts spectrum frames at full rate while
    /// any voting client (this app, the TUI) is focused, and throttles
    /// only when all of them are backgrounded.
    public func setVizFocus(_ focused: Bool) {
        Task { [weak self] in try? await self?.connection.request(.setVizFocus(focused: focused)) }
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
        showToast("Added to queue")
        Task { [weak self] in try? await self?.connection.request(.queueAdd(uri: uri)) }
    }

    /// Append many tracks in one request (e.g. "queue all liked songs").
    public func queueAll(uris: [String]) {
        guard !uris.isEmpty else { return }
        showToast("Added \(uris.count) to queue")
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

    /// Follow an artist. The daemon emits a `LibraryChanged` event that refreshes
    /// the Followed-Artists list.
    public func followArtist(uri: String) {
        showToast("Following")
        Task { [weak self] in try? await self?.connection.request(.artistFollow(artist: uri)) }
    }

    /// Unfollow an artist.
    public func unfollowArtist(uri: String) {
        showToast("Unfollowed")
        Task { [weak self] in try? await self?.connection.request(.artistUnfollow(artist: uri)) }
    }

    // MARK: Like / save

    /// Save (like) a track/album/etc. by URI. The daemon emits `LibraryChanged`.
    public func like(uri: String) {
        showToast("Added to Library")
        Task { [weak self] in
            try? await self?.connection.request(.librarySave(uri: uri, current: false))
        }
    }

    /// Remove a saved (liked) item by URI.
    public func unlike(uri: String) {
        showToast("Removed from Library")
        Task { [weak self] in try? await self?.connection.request(.libraryUnsave(uri: uri)) }
    }

    /// Toggle like for a media item based on its known `inLibrary` state
    /// (defaults to liking when unknown).
    public func toggleLike(_ item: MediaItem) {
        if item.inLibrary == true {
            unlike(uri: item.uri)
        } else {
            like(uri: item.uri)
        }
    }

    /// Like the current now-playing track (no-op when nothing is playing).
    public func likeCurrent() {
        guard let item = player.currentItem else { return }
        toggleLike(item)
    }

    // MARK: Reminders

    /// Schedule a reminder. `anchorAtMs` is an absolute epoch (ms); the tz is the
    /// device's current IANA zone for display + recurrence math.
    public func createReminder(
        uri: String,
        anchorAtMs: Int64,
        recurrence: Recurrence,
        message: String? = nil
    ) {
        let tz = TimeZone.current.identifier
        Task { [weak self] in
            try? await self?.connection.request(
                .reminderCreate(
                    uri: uri, anchorAtMs: anchorAtMs, recurrence: recurrence, tz: tz,
                    message: message))
        }
    }

    public func cancelReminder(id: String) {
        Task { [weak self] in try? await self?.connection.request(.reminderCancel(id: id)) }
    }

    /// Act on an inbox notification (play/queue/snooze/dismiss/seen).
    public func actNotification(id: String, action: String, snoozeUntilMs: Int64? = nil) {
        Task { [weak self] in
            try? await self?.connection.request(
                .notificationAct(id: id, action: action, snoozeUntilMs: snoozeUntilMs))
        }
    }

    public func snoozeNotification(id: String, for interval: TimeInterval) {
        let until = Int64((Date().timeIntervalSince1970 + interval) * 1000)
        actNotification(id: id, action: "snooze", snoozeUntilMs: until)
    }

    /// Bridge an OS-notification action (which only knows the reminder) to the
    /// inbox notification the daemon created when the reminder fired: find the
    /// newest open notification for that reminder and act on it.
    public func actLatestNotification(
        reminderID: String, action: String, snoozeUntilMs: Int64? = nil
    ) {
        Task { [weak self] in
            guard let self else { return }
            guard case .notifications(let list) = try? await self.connection.request(
                .notificationsList(includeArchived: false)) else { return }
            if let match = list.first(where: { $0.reminderID == reminderID && $0.isOpen }) {
                try? await self.connection.request(
                    .notificationAct(id: match.id, action: action, snoozeUntilMs: snoozeUntilMs))
            }
        }
    }

    /// Invoked on the main actor once reminders have loaded after each (re)connect
    /// — the macOS notification scheduler uses this to (re)sync OS notifications.
    public var onRemindersReady: (() -> Void)?

    public func clearBanner() { banner = nil }

    /// Dismiss the update banner (until the next launch / check).
    /// Refused while the updater is mid-flight: clearing
    /// `availableUpdate` hides every surface that could show the
    /// Relaunch button after the bundle swap completes.
    public func dismissUpdate() {
        guard !updater.phase.isBusy else { return }
        availableUpdate = nil
    }

    /// Ask the daemon whether a newer release exists. `force` re-checks now;
    /// otherwise returns the daemon's cached result. Populates `availableUpdate`.
    public func checkUpdate(force: Bool = false) {
        Task { [weak self] in
            guard let self else { return }
            guard case .updateStatus(let status)? =
                try? await self.connection.request(.checkUpdate(force: force)) else { return }
            // Gate on THIS app's version, not the daemon's `update_available`
            // flag: the daemon reports its own (possibly stale) build, so a
            // not-yet-restarted older daemon under a current app would otherwise
            // nag about a release the user already has.
            if let latest = status.latestVersion,
               Self.versionIsNewer(latest, than: self.appVersion) {
                self.availableUpdate = AvailableUpdate(
                    latestVersion: latest,
                    command: status.upgrade.command,
                    url: status.upgrade.url ?? status.releaseURL)
            } else if !self.updater.phase.isBusy {
                // Never yank the update context out from under a
                // mid-flight install/relaunch prompt.
                self.availableUpdate = nil
            }
        }
    }

    /// This app bundle's marketing version (CFBundleShortVersionString).
    public var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? ""
    }

    /// Kick off the one-click update for the currently advertised
    /// release. Progress/result surface via `updater.phase`.
    public func installAvailableUpdate() {
        guard let update = availableUpdate else { return }
        Task { await updater.install(version: update.latestVersion) }
    }

    /// True when `candidate` is a strictly newer dotted version than `current`.
    /// Tolerates a leading `v`; returns false on unparseable input (never nags).
    nonisolated static func versionIsNewer(_ candidate: String, than current: String) -> Bool {
        func parts(_ s: String) -> [Int] {
            s.trimmingCharacters(in: CharacterSet(charactersIn: "v "))
                .split(separator: ".").map { Int($0) ?? 0 }
        }
        let a = parts(candidate), b = parts(current)
        guard !a.isEmpty, !b.isEmpty else { return false }
        for i in 0..<max(a.count, b.count) {
            let x = i < a.count ? a[i] : 0
            let y = i < b.count ? b[i] : 0
            if x != y { return x > y }
        }
        return false
    }

    /// Show a transient confirmation toast that auto-dismisses after ~1.8s.
    public func showToast(_ message: String) {
        toast = message
        toastTask?.cancel()
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.8))
            guard !Task.isCancelled else { return }
            self?.toast = nil
        }
    }

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
        case .reminderDue(let notification):
            banner = "⏰ Reminder: \(notification.name)"
        case .updateAvailable(let latest, let releaseURL, let upgrade):
            // Only surface if THIS app is actually behind (the daemon's event is
            // keyed on its own build, which may lag the installed app).
            if Self.versionIsNewer(latest, than: appVersion) {
                availableUpdate = AvailableUpdate(
                    latestVersion: latest, command: upgrade.command, url: upgrade.url ?? releaseURL)
            }
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
            if attempt == 0 { readiness = .checking }
            debugLog("connecting attempt=\(attempt) path=\(path)")
            let launched = await DaemonLauncher.ensureRunning(socketPath: path)
            do {
                try await connection.connect(to: path)
                // Gate on daemon version BEFORE using v2 features.
                let status = try await fetchDaemonStatus()
                let required = SpotuifyKit.ipcProtocolVersion
                if status.protocolVersion < required {
                    readiness = .incompatible(
                        found: status.protocolVersion,
                        required: required,
                        version: status.daemonVersion ?? "unknown")
                    connectionState = .ready
                    debugLog("incompatible daemon: protocol \(status.protocolVersion) < \(required)")
                    await connection.waitUntilClosed() // recheck if the daemon is replaced
                } else {
                    try await connection.subscribeEvents()
                    try await reseed()
                    readiness = .ready
                    connectionState = .ready
                    banner = nil
                    attempt = 0
                    debugLog("ready (daemon \(status.daemonVersion ?? "?") protocol \(status.protocolVersion))")
                    // One-shot update check (cached on the daemon) so the app
                    // shows an available upgrade even if it missed the push.
                    checkUpdate()
                    await reminders.loadAll()
                    onRemindersReady?()
                    if !dueInboxShown && !reminders.openNotifications.isEmpty {
                        dueInboxShown = true
                        presentDueInbox = true
                    }
                    await connection.waitUntilClosed()
                }
                debugLog("disconnected")
            } catch {
                connectionState = .failed("\(error)")
                if !launched {
                    readiness = .missing(
                        installed: DaemonLauncher.resolveBinary() != nil)
                }
                debugLog("connect failed: \(error)")
            }
            guard !Task.isCancelled else { break }
            attempt += 1
            let delayMs = min(10_000, 250 * (1 << min(attempt, 6)))
            try? await Task.sleep(for: .milliseconds(delayMs))
        }
    }

    private func fetchDaemonStatus() async throws -> DaemonStatus {
        let data = try await connection.request(.getDaemonStatus, timeout: .seconds(8))
        guard case .daemonStatus(let status) = data else {
            throw DaemonConnectionError.unexpectedResponse("expected daemon-status")
        }
        return status
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
        // Gated: this fires for EVERY daemon event, including 30Hz
        // spectrum frames — ungated os_log writes burned CPU/battery
        // and churned the log store around the clock.
        guard debugEnabled else { return }
        logger.notice("\(message, privacy: .public)")
        if let line = "[spotuify] \(message)\n".data(using: .utf8) {
            FileHandle.standardError.write(line) // stderr is unbuffered
        }
    }
}
