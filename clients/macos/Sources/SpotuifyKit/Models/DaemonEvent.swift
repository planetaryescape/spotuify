import Foundation

/// Unsolicited events the daemon broadcasts to subscribed clients, internally
/// tagged by `event` (kebab-case). Events we don't render (mutation receipts,
/// operation log, analytics, …) fall through to `.unknown`,
/// which also future-proofs the client against new event kinds.
public enum DaemonEvent: Decodable, Sendable {
    case playbackChanged(action: String, playback: Playback?)
    case queueChanged(action: String, uris: [String], queue: Queue?)
    case devicesChanged(action: String, devices: [Device]?)
    case playlistsChanged(action: String, playlist: String?, provider: ProviderID?)
    case libraryChanged(action: String, uris: [String], provider: ProviderID?)
    case searchUpdated(query: String, count: Int, provider: ProviderID?)
    case searchPage(
        query: String, kind: MediaKind, offset: UInt32, version: UInt64,
        items: [MediaItem], provider: ProviderID?)
    case searchComplete(query: String, version: UInt64, provider: ProviderID?)
    case searchFailed(
        query: String, version: UInt64, kind: MediaKind?, offset: UInt32?,
        message: String, provider: ProviderID?)
    case syncStarted(target: SyncTarget, provider: ProviderID?)
    case syncFinished(CacheSyncSummary)
    case eventStreamLagged(skipped: UInt64)
    case rateLimited(retryAfterSecs: UInt64, scope: String, provider: ProviderID?)
    case authError(kind: String, provider: ProviderID?)
    case playerReady(deviceID: String, name: String)
    case playerDegraded(reason: String)
    case providerPolicy(provider: ProviderID, reason: String)
    case providerPolicyCleared(provider: ProviderID, reason: String)
    /// Compatibility with released daemons. New daemons emit providerPolicy.
    case premiumRequired
    case sessionDisconnected(reason: String)
    case playerFailed(reason: String, restarts: UInt32)
    case spectrumFrame(bands: [Float], peak: Float, timestampMs: UInt64)
    case configReloaded
    case shutdownRequested
    case reminderDue(ReminderNotification)
    case remindersChanged(action: String)
    case updateAvailable(latestVersion: String, releaseURL: String?, upgrade: UpgradeHint)
    case authMigrationRecommended(canLoginDevApp: Bool)
    case unknown(event: String)

    private enum CodingKeys: String, CodingKey {
        case event, action, playback, uris, queue, devices, playlist, provider, target, summary
        case query, count, kind, offset, version, items, skipped
        case retryAfterSecs = "retry_after_secs"
        case scope, reason, restarts, name, bands, peak, message
        case deviceID = "device_id"
        case timestampMs = "timestamp_ms"
        case notification, upgrade
        case latestVersion = "latest_version"
        case releaseURL = "release_url"
        case canLoginDevApp = "can_login_dev_app"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let event = try c.decode(String.self, forKey: .event)
        switch event {
        case "playback-changed":
            self = .playbackChanged(
                action: try c.decode(String.self, forKey: .action),
                playback: try c.decodeIfPresent(Playback.self, forKey: .playback))
        case "queue-changed":
            self = .queueChanged(
                action: try c.decode(String.self, forKey: .action),
                uris: try c.decodeIfPresent([String].self, forKey: .uris) ?? [],
                queue: try c.decodeIfPresent(Queue.self, forKey: .queue))
        case "devices-changed":
            self = .devicesChanged(
                action: try c.decode(String.self, forKey: .action),
                devices: try c.decodeIfPresent([Device].self, forKey: .devices))
        case "playlists-changed":
            self = .playlistsChanged(
                action: try c.decode(String.self, forKey: .action),
                playlist: try c.decodeIfPresent(String.self, forKey: .playlist),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "library-changed":
            self = .libraryChanged(
                action: try c.decode(String.self, forKey: .action),
                uris: try c.decodeIfPresent([String].self, forKey: .uris) ?? [],
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "search-updated":
            self = .searchUpdated(
                query: try c.decode(String.self, forKey: .query),
                count: try c.decode(Int.self, forKey: .count),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "search-page":
            self = .searchPage(
                query: try c.decode(String.self, forKey: .query),
                kind: try c.decode(MediaKind.self, forKey: .kind),
                offset: try c.decode(UInt32.self, forKey: .offset),
                version: try c.decode(UInt64.self, forKey: .version),
                items: try c.decode([MediaItem].self, forKey: .items),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "search-complete":
            self = .searchComplete(
                query: try c.decode(String.self, forKey: .query),
                version: try c.decode(UInt64.self, forKey: .version),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "search-failed":
            self = .searchFailed(
                query: try c.decode(String.self, forKey: .query),
                version: try c.decode(UInt64.self, forKey: .version),
                kind: try c.decodeIfPresent(MediaKind.self, forKey: .kind),
                offset: try c.decodeIfPresent(UInt32.self, forKey: .offset),
                message: try c.decodeIfPresent(String.self, forKey: .message) ?? "",
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "sync-started":
            self = .syncStarted(
                target: try c.decode(SyncTarget.self, forKey: .target),
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "sync-finished":
            self = .syncFinished(try c.decode(CacheSyncSummary.self, forKey: .summary))
        case "event-stream-lagged":
            self = .eventStreamLagged(skipped: try c.decode(UInt64.self, forKey: .skipped))
        case "rate-limited":
            self = .rateLimited(
                retryAfterSecs: try c.decode(UInt64.self, forKey: .retryAfterSecs),
                scope: try c.decodeIfPresent(String.self, forKey: .scope) ?? "",
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "auth-error":
            self = .authError(
                kind: (try? c.decode(String.self, forKey: .kind)) ?? "unknown",
                provider: try c.decodeIfPresent(ProviderID.self, forKey: .provider))
        case "player-ready":
            self = .playerReady(
                deviceID: try c.decodeIfPresent(String.self, forKey: .deviceID) ?? "",
                name: try c.decodeIfPresent(String.self, forKey: .name) ?? "")
        case "player-degraded":
            self = .playerDegraded(reason: try c.decodeIfPresent(String.self, forKey: .reason) ?? "")
        case "provider-policy":
            self = .providerPolicy(
                provider: try c.decode(ProviderID.self, forKey: .provider),
                reason: try c.decode(String.self, forKey: .reason))
        case "provider-policy-cleared":
            self = .providerPolicyCleared(
                provider: try c.decode(ProviderID.self, forKey: .provider),
                reason: try c.decode(String.self, forKey: .reason))
        case "premium-required":
            self = .premiumRequired
        case "session-disconnected":
            self = .sessionDisconnected(reason: try c.decodeIfPresent(String.self, forKey: .reason) ?? "")
        case "player-failed":
            self = .playerFailed(
                reason: try c.decodeIfPresent(String.self, forKey: .reason) ?? "",
                restarts: try c.decodeIfPresent(UInt32.self, forKey: .restarts) ?? 0)
        case "spectrum-frame":
            self = .spectrumFrame(
                bands: try c.decode([Float].self, forKey: .bands),
                peak: try c.decodeIfPresent(Float.self, forKey: .peak) ?? 0,
                timestampMs: try c.decodeIfPresent(UInt64.self, forKey: .timestampMs) ?? 0)
        case "config-reloaded":
            self = .configReloaded
        case "shutdown-requested":
            self = .shutdownRequested
        case "reminder-due":
            self = .reminderDue(try c.decode(ReminderNotification.self, forKey: .notification))
        case "reminders-changed":
            self = .remindersChanged(action: try c.decodeIfPresent(String.self, forKey: .action) ?? "")
        case "update-available":
            self = .updateAvailable(
                latestVersion: try c.decode(String.self, forKey: .latestVersion),
                releaseURL: try c.decodeIfPresent(String.self, forKey: .releaseURL),
                upgrade: try c.decode(UpgradeHint.self, forKey: .upgrade))
        case "auth-migration-recommended":
            self = .authMigrationRecommended(
                canLoginDevApp: try c.decodeIfPresent(Bool.self, forKey: .canLoginDevApp) ?? false)
        default:
            self = .unknown(event: event)
        }
    }
}
