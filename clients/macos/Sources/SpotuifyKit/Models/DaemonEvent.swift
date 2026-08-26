import Foundation

/// Unsolicited events the daemon broadcasts to subscribed clients, internally
/// tagged by `event` (kebab-case).
///
/// Every kind the Rust `DaemonEvent` defines has a case here — `handledEventTags`
/// and the `event-kinds.json` fixture hold both sides to that, in both
/// directions. `.unknown` is the forward-compat fallback for a daemon newer than
/// this app, not a place to park kinds we didn't feel like modelling.
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
    case clientPreferencesChanged(ClientPreferences)
    case shutdownRequested
    case reminderDue(ReminderNotification)
    case remindersChanged(action: String)
    case bookmarksChanged(action: String)
    case eqChanged(settings: EqSettings, applied: Bool)
    case updateAvailable(latestVersion: String, releaseURL: String?, upgrade: UpgradeHint)
    case authMigrationRecommended(canLoginDevApp: Bool)
    case mutationFinished(action: String, message: String)
    case mutationAccepted(receiptID: String, action: String)
    case mutationFinalized(receiptID: String, status: String, message: String)
    case schemaCompat(endpoint: String, missingKeys: [String])
    case listenQualified(trackURI: String, durationMs: Int64, audibleMs: Int64)
    case analyticsImportProgress(runID: String, phase: String, fetched: UInt64, stored: UInt64)
    case operationRecorded(operationID: String, kind: String, source: String)
    case operationUndone(undoOpID: String, originalOpID: String, success: Bool)
    case vizSourceChanged(active: String, configured: String, hint: String?)
    case unknown(event: String)

    /// Every tag `init(from:)` decodes into a real case. Kept beside the switch
    /// below; `ProtocolParityTests` proves each entry is genuinely handled and
    /// that the set matches the Rust roster.
    public static let handledEventTags: Set<String> = [
        "analytics-import-progress",
        "auth-error",
        "auth-migration-recommended",
        "bookmarks-changed",
        "client-preferences-changed",
        "config-reloaded",
        "devices-changed",
        "eq-changed",
        "event-stream-lagged",
        "library-changed",
        "listen-qualified",
        "mutation-accepted",
        "mutation-finalized",
        "mutation-finished",
        "operation-recorded",
        "operation-undone",
        "playback-changed",
        "player-degraded",
        "player-failed",
        "player-ready",
        "playlists-changed",
        "premium-required",
        "provider-policy",
        "provider-policy-cleared",
        "queue-changed",
        "rate-limited",
        "reminder-due",
        "reminders-changed",
        "schema-compat",
        "search-complete",
        "search-failed",
        "search-page",
        "search-updated",
        "session-disconnected",
        "shutdown-requested",
        "spectrum-frame",
        "sync-finished",
        "sync-started",
        "update-available",
        "viz-source-changed",
    ]

    private enum CodingKeys: String, CodingKey {
        case event, action, playback, uris, queue, devices, playlist, provider, target, summary
        case query, count, kind, offset, version, items, skipped
        case retryAfterSecs = "retry_after_secs"
        case scope, reason, restarts, name, bands, peak, message, settings, applied
        case deviceID = "device_id"
        case timestampMs = "timestamp_ms"
        case notification, upgrade, preferences
        case endpoint, phase, fetched, stored, status, success, active, configured, hint, source
        case receiptID = "receipt_id"
        case missingKeys = "missing_keys"
        case trackURI = "track_uri"
        case durationMs = "duration_ms"
        case audibleMs = "audible_ms"
        case runID = "run_id"
        case operationID = "operation_id"
        case undoOpID = "undo_op_id"
        case originalOpID = "original_op_id"
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
        case "client-preferences-changed":
            self = .clientPreferencesChanged(
                try c.decode(ClientPreferences.self, forKey: .preferences))
        case "shutdown-requested":
            self = .shutdownRequested
        case "reminder-due":
            self = .reminderDue(try c.decode(ReminderNotification.self, forKey: .notification))
        case "reminders-changed":
            self = .remindersChanged(action: try c.decodeIfPresent(String.self, forKey: .action) ?? "")
        case "bookmarks-changed":
            self = .bookmarksChanged(action: try c.decodeIfPresent(String.self, forKey: .action) ?? "")
        case "eq-changed":
            self = .eqChanged(
                settings: try c.decode(EqSettings.self, forKey: .settings),
                applied: try c.decodeIfPresent(Bool.self, forKey: .applied) ?? false)
        case "update-available":
            self = .updateAvailable(
                latestVersion: try c.decode(String.self, forKey: .latestVersion),
                releaseURL: try c.decodeIfPresent(String.self, forKey: .releaseURL),
                upgrade: try c.decode(UpgradeHint.self, forKey: .upgrade))
        case "auth-migration-recommended":
            self = .authMigrationRecommended(
                canLoginDevApp: try c.decodeIfPresent(Bool.self, forKey: .canLoginDevApp) ?? false)
        case "mutation-finished":
            self = .mutationFinished(
                action: try c.decodeIfPresent(String.self, forKey: .action) ?? "",
                message: try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        case "mutation-accepted":
            self = .mutationAccepted(
                receiptID: try c.decode(String.self, forKey: .receiptID),
                action: try c.decodeIfPresent(String.self, forKey: .action) ?? "")
        case "mutation-finalized":
            self = .mutationFinalized(
                receiptID: try c.decode(String.self, forKey: .receiptID),
                status: try c.decodeIfPresent(String.self, forKey: .status) ?? "",
                message: try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        case "schema-compat":
            self = .schemaCompat(
                endpoint: try c.decode(String.self, forKey: .endpoint),
                missingKeys: try c.decodeIfPresent([String].self, forKey: .missingKeys) ?? [])
        case "listen-qualified":
            self = .listenQualified(
                trackURI: try c.decode(String.self, forKey: .trackURI),
                durationMs: try c.decodeIfPresent(Int64.self, forKey: .durationMs) ?? 0,
                audibleMs: try c.decodeIfPresent(Int64.self, forKey: .audibleMs) ?? 0)
        case "analytics-import-progress":
            self = .analyticsImportProgress(
                runID: try c.decode(String.self, forKey: .runID),
                phase: try c.decodeIfPresent(String.self, forKey: .phase) ?? "",
                fetched: try c.decodeIfPresent(UInt64.self, forKey: .fetched) ?? 0,
                stored: try c.decodeIfPresent(UInt64.self, forKey: .stored) ?? 0)
        case "operation-recorded":
            self = .operationRecorded(
                operationID: try c.decode(String.self, forKey: .operationID),
                kind: try c.decodeIfPresent(String.self, forKey: .kind) ?? "",
                source: try c.decodeIfPresent(String.self, forKey: .source) ?? "")
        case "operation-undone":
            self = .operationUndone(
                undoOpID: try c.decode(String.self, forKey: .undoOpID),
                originalOpID: try c.decode(String.self, forKey: .originalOpID),
                success: try c.decodeIfPresent(Bool.self, forKey: .success) ?? false)
        case "viz-source-changed":
            self = .vizSourceChanged(
                active: try c.decode(String.self, forKey: .active),
                configured: try c.decodeIfPresent(String.self, forKey: .configured) ?? "",
                hint: try c.decodeIfPresent(String.self, forKey: .hint))
        default:
            self = .unknown(event: event)
        }
    }
}
