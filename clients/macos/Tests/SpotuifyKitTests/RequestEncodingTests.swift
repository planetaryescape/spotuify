import Foundation
import Testing
@testable import SpotuifyKit

private actor MutationRetryCacheHarness {
    private var cache = MutationRetryCache()
    private var inFlight: [UUID: PreparedDaemonAttempt] = [:]

    func start(_ request: DaemonRequest) throws -> UUID {
        let attempt = try cache.attempt(for: request)
        guard let mutationID = attempt.prepared.mutationID else {
            preconditionFailure("test harness requires a mutation request")
        }
        inFlight[mutationID] = attempt
        return mutationID
    }

    func finish(_ mutationID: UUID, uncertainOutcome: Bool) {
        guard let attempt = inFlight.removeValue(forKey: mutationID) else {
            preconditionFailure("unknown in-flight mutation")
        }
        cache.finish(attempt, uncertainOutcome: uncertainOutcome)
    }
}

@Suite("Request encoding")
struct RequestEncodingTests {
    /// Encode an outbound request and return its JSON as a dictionary tree so
    /// assertions are order-independent.
    private func encode(_ request: DaemonRequest, id: UInt64 = 1) throws -> [String: Any] {
        let data = try Wire.encodeOutbound(OutboundMessage(id: id, request: request))
        return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private func payload(_ request: DaemonRequest) throws -> [String: Any] {
        let root = try encode(request)
        return try #require(root["payload"] as? [String: Any])
    }

    @Test("ping carries the right envelope")
    func ping() throws {
        let root = try encode(.ping, id: 42)
        #expect(root["id"] as? UInt64 == 42 || (root["id"] as? NSNumber)?.intValue == 42)
        let payload = try #require(root["payload"] as? [String: Any])
        #expect(payload["type"] as? String == "Request")
        #expect(payload["cmd"] as? String == "ping")
    }

    @Test("event subscription declares provider-policy capability")
    func subscribeEventsCapability() throws {
        let request = try payload(.subscribeEvents)
        #expect(request["cmd"] as? String == "subscribe-events")
        #expect(request["provider_policy"] as? Bool == true)
    }

    @Test("protected writes carry one stable caller-owned mutation id")
    func mutationId() throws {
        let id = try #require(UUID(uuidString: "018f47d2-9e2a-7000-8000-000000000001"))
        let data = try Wire.encodeOutbound(
            OutboundMessage(id: 7, request: .queueAdd(uri: "spotify:track:1"), mutationId: id))
        let root = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(root["mutation_id"] as? String == id.uuidString)

        let generated = try encode(.queueAdd(uri: "spotify:track:2"))
        let generatedID = try #require(generated["mutation_id"] as? String)
        let versionGroup = try #require(generatedID.split(separator: "-").dropFirst(2).first)
        #expect(versionGroup.first == "7")
    }

    @Test("prepared mutation retries encode the same caller-owned id")
    func preparedMutationRetryID() throws {
        let prepared = PreparedDaemonRequest(.queueAdd(uri: "spotify:track:retry"))
        let firstData = try Wire.encodeOutbound(OutboundMessage(id: 1, prepared: prepared))
        let secondData = try Wire.encodeOutbound(OutboundMessage(id: 2, prepared: prepared))
        let first = try #require(
            JSONSerialization.jsonObject(with: firstData) as? [String: Any])
        let second = try #require(
            JSONSerialization.jsonObject(with: secondData) as? [String: Any])

        let mutationID = try #require(first["mutation_id"] as? String)
        #expect(second["mutation_id"] as? String == mutationID)
        #expect((first["id"] as? NSNumber)?.intValue == 1)
        #expect((second["id"] as? NSNumber)?.intValue == 2)
    }

    @Test("wrapper timeout retry reuses one logical write then success rotates id")
    func wrapperMutationRetryLifecycle() throws {
        var cache = MutationRetryCache()
        let request = DaemonRequest.queueAdd(uri: "spotify:track:retry")
        let first = try cache.attempt(for: request)
        cache.finish(first, uncertainOutcome: true)
        let retry = try cache.attempt(for: request)

        let firstID = try #require(first.prepared.mutationID)
        #expect(retry.prepared.mutationID == firstID)
        let firstData = try Wire.encodeOutbound(OutboundMessage(id: 1, prepared: first.prepared))
        let retryData = try Wire.encodeOutbound(OutboundMessage(id: 2, prepared: retry.prepared))
        let firstFrame = try #require(
            JSONSerialization.jsonObject(with: firstData) as? [String: Any])
        let retryFrame = try #require(
            JSONSerialization.jsonObject(with: retryData) as? [String: Any])
        let logicalWrites = Set([
            try #require(firstFrame["mutation_id"] as? String),
            try #require(retryFrame["mutation_id"] as? String),
        ])
        #expect(logicalWrites.count == 1)

        cache.finish(retry, uncertainOutcome: false)
        let afterSuccess = try cache.attempt(for: request)
        #expect(afterSuccess.prepared.mutationID != firstID)
    }

    @Test("concurrent identical writes retain only the exact uncertain attempt")
    func concurrentIdenticalMutationRetryLifecycle() async throws {
        let cache = MutationRetryCacheHarness()
        let request = DaemonRequest.queueAdd(uri: "spotify:track:concurrent")

        async let firstStart = cache.start(request)
        async let secondStart = cache.start(request)
        let firstID = try await firstStart
        let secondID = try await secondStart
        #expect(firstID != secondID)

        await cache.finish(firstID, uncertainOutcome: true)
        await cache.finish(secondID, uncertainOutcome: false)

        let retryID = try await cache.start(request)
        #expect(retryID == firstID)
        await cache.finish(retryID, uncertainOutcome: false)

        let nextLogicalWriteID = try await cache.start(request)
        #expect(nextLogicalWriteID != firstID)
        #expect(nextLogicalWriteID != secondID)
    }

    @Test("only a lost transport response retains a mutation retry id")
    func mutationRetryErrorClassification() throws {
        #expect(DaemonConnection.shouldRetainMutationAttempt(after: DaemonConnectionError.timeout))
        #expect(
            DaemonConnection.shouldRetainMutationAttempt(
                after: DaemonConnectionError.disconnected))
        #expect(
            !DaemonConnection.shouldRetainMutationAttempt(
                after: DaemonConnectionError.notConnected))

        let encodingError = EncodingError.invalidValue(
            "injected",
            EncodingError.Context(codingPath: [], debugDescription: "injected"))
        #expect(!DaemonConnection.shouldRetainMutationAttempt(after: encodingError))

        let request = DaemonRequest.queueAdd(uri: "spotify:track:not-sent")
        for error in [DaemonConnectionError.notConnected, encodingError as Error] {
            var cache = MutationRetryCache()
            let first = try cache.attempt(for: request)
            let firstID = try #require(first.prepared.mutationID)
            cache.finish(
                first,
                uncertainOutcome: DaemonConnection.shouldRetainMutationAttempt(after: error))
            let retry = try cache.attempt(for: request)
            #expect(retry.prepared.mutationID != firstID)
        }
    }

    @Test("an ambiguous retry key survives a later pre-send failure")
    func ambiguousRetrySurvivesPreSendFailure() throws {
        var cache = MutationRetryCache()
        let request = DaemonRequest.queueAdd(uri: "spotify:track:retry-after-reconnect")

        let first = try cache.attempt(for: request)
        let firstID = try #require(first.prepared.mutationID)
        cache.finish(first, disposition: .uncertain)

        let disconnectedRetry = try cache.attempt(for: request)
        #expect(disconnectedRetry.prepared.mutationID == firstID)
        cache.finish(disconnectedRetry, disposition: .notSent)

        let connectedRetry = try cache.attempt(for: request)
        #expect(connectedRetry.prepared.mutationID == firstID)
        cache.finish(connectedRetry, disposition: .definitive)

        let nextLogicalWrite = try cache.attempt(for: request)
        #expect(nextLogicalWrite.prepared.mutationID != firstID)
    }

    @Test("a retained retry key expires after the retention TTL")
    func mutationRetryKeyExpires() throws {
        final class MutableClock {
            var value: Date
            init(_ value: Date) { self.value = value }
        }
        let clock = MutableClock(Date(timeIntervalSince1970: 1_000))
        var cache = MutationRetryCache(now: { clock.value })
        let request = DaemonRequest.queueAdd(uri: "spotify:track:ttl")

        let first = try cache.attempt(for: request)
        let firstID = try #require(first.prepared.mutationID)
        cache.finish(first, uncertainOutcome: true)

        // Within the TTL: an identical request reuses the same logical write.
        clock.value.addTimeInterval(MutationRetryCache.retentionTTL - 1)
        let withinTTL = try cache.attempt(for: request)
        #expect(withinTTL.prepared.mutationID == firstID)
        cache.finish(withinTTL, uncertainOutcome: true)

        // Past the TTL: the stale key is abandoned and a fresh id is minted so
        // the daemon cannot replay the old receipt for a new logical write.
        clock.value.addTimeInterval(MutationRetryCache.retentionTTL + 1)
        let expired = try cache.attempt(for: request)
        #expect(expired.prepared.mutationID != firstID)
        #expect(expired.wasUncertain == false)
        #expect(cache.count == 0)
    }

    @Test("mutation retry cache evicts the deterministic oldest entry at its bound")
    func mutationRetryCacheBound() throws {
        var cache = MutationRetryCache()
        let firstRequest = DaemonRequest.queueAdd(uri: "spotify:track:0")
        let firstAttempt = try cache.attempt(for: firstRequest)
        let firstID = try #require(firstAttempt.prepared.mutationID)
        cache.finish(firstAttempt, uncertainOutcome: true)

        var lastRequest = firstRequest
        var lastID = firstID
        for index in 1...MutationRetryCache.capacity {
            lastRequest = .queueAdd(uri: "spotify:track:\(index)")
            let attempt = try cache.attempt(for: lastRequest)
            lastID = try #require(attempt.prepared.mutationID)
            cache.finish(attempt, uncertainOutcome: true)
        }

        #expect(cache.count == MutationRetryCache.capacity)
        let afterEviction = try cache.attempt(for: firstRequest)
        #expect(afterEviction.prepared.mutationID != firstID)
        let retainedNewest = try cache.attempt(for: lastRequest)
        #expect(retainedNewest.prepared.mutationID == lastID)
    }

    @Test("reads omit mutation id")
    func readOmitsMutationId() throws {
        #expect(try encode(.ping)["mutation_id"] == nil)
        #expect(try encode(.radioStart(seedUri: "spotify:track:1", dryRun: true))["mutation_id"] == nil)
        #expect(try encode(.opsUndo(dryRun: true))["mutation_id"] == nil)
    }

    @Test("conditional mutations carry retry ids only when live")
    func conditionalMutationIDs() throws {
        #expect(try encode(.radioStart(seedUri: "spotify:track:1"))["mutation_id"] != nil)
        #expect(try encode(.opsUndo())["mutation_id"] != nil)
        #expect(try encode(.opsRedo())["mutation_id"] != nil)
    }

    @Test("auth sessions encode optional selectors and UUID ids")
    func authSessions() throws {
        let sessionID = try #require(UUID(uuidString: "018f47d2-9e2a-7000-8000-000000000001"))
        let start = try payload(.authStart(provider: .spotify, method: "dev_app"))
        #expect(start["cmd"] as? String == "auth-start")
        #expect(start["provider"] as? String == "spotify")
        #expect(start["method"] as? String == "dev_app")

        let poll = try payload(.authPoll(sessionId: sessionID))
        #expect(poll["cmd"] as? String == "auth-poll")
        #expect(poll["session_id"] as? String == sessionID.uuidString)

        let cancel = try payload(.authCancel(sessionId: sessionID))
        #expect(cancel["cmd"] as? String == "auth-cancel")
        #expect(cancel["session_id"] as? String == sessionID.uuidString)

        let status = try payload(.authStatus(provider: .spotify))
        #expect(status["cmd"] as? String == "auth-status")
        #expect(status["provider"] as? String == "spotify")

        let logout = try payload(.authLogout(provider: .spotify))
        #expect(logout["cmd"] as? String == "auth-logout")
        #expect(logout["provider"] as? String == "spotify")
    }

    @Test("unit playback command serializes as a bare string")
    func pauseCommand() throws {
        let payload = try payload(.playbackCommand(.pause))
        #expect(payload["cmd"] as? String == "playback-command")
        #expect(payload["command"] as? String == "pause")
    }

    @Test("seek serializes as a single-key object with position_ms")
    func seekCommand() throws {
        let payload = try payload(.playbackCommand(.seek(positionMs: 1234)))
        let command = try #require(payload["command"] as? [String: Any])
        let seek = try #require(command["seek"] as? [String: Any])
        #expect((seek["position_ms"] as? NSNumber)?.intValue == 1234)
    }

    @Test("play-uri serializes with the hyphenated tag and uri field")
    func playURICommand() throws {
        let payload = try payload(.playbackCommand(.playURI("spotify:track:xyz")))
        let command = try #require(payload["command"] as? [String: Any])
        let playURI = try #require(command["play-uri"] as? [String: Any])
        #expect(playURI["uri"] as? String == "spotify:track:xyz")
    }

    @Test("volume / shuffle / repeat serialize with their state fields")
    func stateCommands() throws {
        let volume = try #require(try payload(.playbackCommand(.volume(percent: 60)))["command"] as? [String: Any])
        #expect(((volume["volume"] as? [String: Any])?["volume_percent"] as? NSNumber)?.intValue == 60)

        let shuffle = try #require(try payload(.playbackCommand(.shuffle(true)))["command"] as? [String: Any])
        #expect(((shuffle["shuffle"] as? [String: Any])?["state"] as? NSNumber)?.boolValue == true)

        let repeatCmd = try #require(try payload(.playbackCommand(.repeatMode(.context)))["command"] as? [String: Any])
        #expect((repeatCmd["repeat"] as? [String: Any])?["state"] as? String == "context")
    }

    @Test("search carries query, lowercase scope/source, and limit")
    func searchRequest() throws {
        let payload = try payload(.search(query: "miles davis", scope: .album, source: .spotify, limit: 20))
        #expect(payload["cmd"] as? String == "search")
        #expect(payload["query"] as? String == "miles davis")
        #expect(payload["scope"] as? String == "album")
        #expect(payload["source"] as? String == "spotify")
        #expect((payload["limit"] as? NSNumber)?.intValue == 20)
    }

    @Test("provider discovery requests match the frozen v7 wire")
    func providerRequests() throws {
        #expect(try payload(.providersList)["cmd"] as? String == "providers-list")
        #expect(try payload(.listAudioOutputs)["cmd"] as? String == "list-audio-outputs")

        let resolved = try payload(
            .resolveTarget(
                input: "https://music.apple.com/song/1", provider: nil,
                expectedKinds: [.track, .episode]))
        #expect(resolved["cmd"] as? String == "resolve-target")
        #expect(resolved["input"] as? String == "https://music.apple.com/song/1")
        #expect(resolved["provider"] == nil)
        #expect(resolved["expected_kinds"] as? [String] == ["track", "episode"])
    }

    @Test("search-source keeps legacy spotify scalar and extends remote providers")
    func remoteSearchSource() throws {
        let apple = try #require(ProviderID(rawValue: "apple"))
        let remote = try payload(
            .search(
                query: "miles", scope: .track, source: .remote(apple), limit: 20,
                provider: apple))
        #expect((remote["source"] as? [String: String])?["remote"] == "apple")
        #expect(remote["provider"] as? String == "apple")

        let decoded = try JSONDecoder().decode(SearchSource.self, from: Data(#""spotify""#.utf8))
        #expect(decoded == .spotify)
        #expect(try JSONEncoder().encode(decoded) == Data(#""spotify""#.utf8))
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(SearchSource.self, from: Data(#"{"remote":"apple","extra":1}"#.utf8))
        }
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(SearchSource.self, from: Data(#""unknown""#.utf8))
        }
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(ProviderID.self, from: Data(#""Spotify""#.utf8))
        }
    }

    @Test("provider-scoped requests encode provider and legacy calls omit it")
    func providerScopedRequests() throws {
        let scoped: [DaemonRequest] = [
            .playlistsList(provider: .spotify), .recentlyPlayed(provider: .spotify),
            .libraryList(limit: 20, provider: .spotify),
            .savedTracks(limit: 20, offset: 0, provider: .spotify),
            .savedShows(limit: 20, provider: .spotify),
            .followedArtists(limit: 20, provider: .spotify),
            .episodeFeed(limit: 20, sort: .newest, refresh: false, provider: .spotify),
            .sync(target: .all, provider: .spotify),
            .playlistCreate(name: "Mix", uris: [], provider: .spotify),
            .playlistCreatePreview(
                name: "Mix", description: "Deep focus", uris: ["spotify:track:one"],
                provider: .spotify),
            .playlistTracks(playlist: "spotify:playlist:mix", wait: true, provider: .spotify),
            .playlistAddItems(
                playlist: "spotify:playlist:mix", uris: ["spotify:track:one"],
                provider: .spotify),
            .playlistItemsPreview(
                playlist: "spotify:playlist:mix", uris: ["spotify:track:one"],
                action: .remove, provider: .spotify),
            .playlistRemoveItems(
                playlist: "spotify:playlist:mix", uris: ["spotify:track:one"],
                provider: .spotify),
            .playlistSetImage(
                playlist: "spotify:playlist:mix", imageBase64: "jpeg", provider: .spotify),
            .playlistUnfollow(playlist: "spotify:playlist:mix", provider: .spotify),
        ]
        for request in scoped {
            #expect(try payload(request)["provider"] as? String == "spotify")
        }
        #expect(try payload(.playlistsList())["provider"] == nil)
        #expect(try payload(.sync(target: .all))["provider"] == nil)
        #expect(try payload(.playlistCreatePreview(name: "Mix", uris: []))["provider"] == nil)
        let createPreview = try payload(
            .playlistCreatePreview(
                name: "Mix", description: "Deep focus", uris: ["spotify:track:one"]))
        #expect(createPreview["description"] as? String == "Deep focus")
        #expect(try payload(.playlistTracks(playlist: "mix", wait: false))["provider"] == nil)
        #expect(try payload(.playlistAddItems(playlist: "mix", uris: []))["provider"] == nil)
        let preview = try payload(
            .playlistItemsPreview(playlist: "mix", uris: ["spotify:track:one"], action: .remove))
        #expect(preview["provider"] == nil)
        #expect(preview["action"] as? String == "remove")
        #expect(try payload(.playlistRemoveItems(playlist: "mix", uris: []))["provider"] == nil)
        #expect(try payload(.playlistSetImage(playlist: "mix", imageBase64: "jpeg"))["provider"] == nil)
        #expect(try payload(.playlistUnfollow(playlist: "mix"))["provider"] == nil)
    }

    @Test("device-transfer carries the device field")
    func deviceTransfer() throws {
        let payload = try payload(.deviceTransfer(device: "spotuify-hume"))
        #expect(payload["cmd"] as? String == "device-transfer")
        #expect(payload["device"] as? String == "spotuify-hume")
    }

    @Test("cover-art carries the Spotify image URL")
    func coverArt() throws {
        let payload = try payload(.coverArt(url: "https://i.scdn.co/image/abc"))
        #expect(payload["cmd"] as? String == "cover-art")
        #expect(payload["url"] as? String == "https://i.scdn.co/image/abc")
    }

    @Test("library-save omits uri when nil and sets current")
    func librarySaveCurrent() throws {
        let payload = try payload(.librarySave(uri: nil, current: true))
        #expect(payload["cmd"] as? String == "library-save")
        #expect(payload["uri"] == nil)
        #expect((payload["current"] as? NSNumber)?.boolValue == true)
    }

    @Test("v2 library/podcast/queue requests encode to the right cmd + fields")
    func v2Requests() throws {
        let saved = try payload(.savedTracks(limit: 50, offset: 10))
        #expect(saved["cmd"] as? String == "saved-tracks")
        #expect((saved["limit"] as? NSNumber)?.intValue == 50)
        #expect((saved["offset"] as? NSNumber)?.intValue == 10)

        let shows = try payload(.savedShows(limit: 200))
        #expect(shows["cmd"] as? String == "saved-shows")

        let episodes = try payload(.showEpisodes(show: "spotify:show:x", limit: 50, offset: 0))
        #expect(episodes["cmd"] as? String == "show-episodes")
        #expect(episodes["show"] as? String == "spotify:show:x")

        let album = try payload(.albumTracks(album: "spotify:album:a"))
        #expect(album["cmd"] as? String == "album-tracks")
        #expect(album["album"] as? String == "spotify:album:a")

        let artist = try payload(.artistAlbums(artist: "spotify:artist:a"))
        #expect(artist["cmd"] as? String == "artist-albums")

        let queueMany = try payload(.queueAddMany(uris: ["spotify:track:1", "spotify:track:2"]))
        #expect(queueMany["cmd"] as? String == "queue-add-many")
        #expect((queueMany["uris"] as? [Any])?.count == 2)
    }

    @Test("reminder + notification requests encode to the right cmd + fields")
    func reminderRequests() throws {
        let create = try payload(.reminderCreate(
            uri: "spotify:album:a", anchorAtMs: 1_700_000_000_000, recurrence: .weekly,
            tz: "America/New_York", message: "revisit"))
        #expect(create["cmd"] as? String == "reminder-create")
        #expect(create["media_uri"] as? String == "spotify:album:a")
        #expect((create["anchor_at_ms"] as? NSNumber)?.int64Value == 1_700_000_000_000)
        #expect(create["recurrence"] as? String == "weekly")
        #expect(create["tz"] as? String == "America/New_York")
        #expect(create["message"] as? String == "revisit")

        let list = try payload(.remindersList(includeInactive: true))
        #expect(list["cmd"] as? String == "reminders-list")
        #expect((list["include_inactive"] as? NSNumber)?.boolValue == true)

        let cancel = try payload(.reminderCancel(id: "r1"))
        #expect(cancel["cmd"] as? String == "reminder-cancel")
        #expect(cancel["id"] as? String == "r1")

        let notifications = try payload(.notificationsList(includeArchived: false))
        #expect(notifications["cmd"] as? String == "notifications-list")

        let act = try payload(.notificationAct(id: "n1", action: "snooze", snoozeUntilMs: 1_700_000_900_000))
        #expect(act["cmd"] as? String == "notification-act")
        #expect(act["id"] as? String == "n1")
        #expect(act["action"] as? String == "snooze")
        #expect((act["snooze_until_ms"] as? NSNumber)?.int64Value == 1_700_000_900_000)
    }
}
