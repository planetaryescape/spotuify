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
        model.handle(.rateLimited(retryAfterSecs: 5, scope: "search", provider: .spotify))
        #expect(model.banner?.contains("5s") == true)
    }

    @Test("provider-policy event surfaces provider-neutral reason")
    func providerPolicyBanner() throws {
        let model = AppModel()
        let provider = try #require(ProviderID(rawValue: "nebula"))

        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))

        #expect(model.banner == "nebula local playback unavailable: region restricted")
        #expect(model.banner?.contains("Spotify") == false)
        #expect(model.banner?.contains("Premium") == false)
    }

    @Test("legacy premium-required event does not invent provider identity")
    func legacyPremiumRequiredBannerIsProviderNeutral() {
        let model = AppModel()

        model.handle(.premiumRequired)

        #expect(model.banner == "Local playback unavailable: account tier does not permit local playback")
        #expect(model.banner?.contains("Spotify") == false)
    }

    @Test("player ready preserves provider-policy banner")
    func playerReadyPreservesProviderPolicyBanner() throws {
        let model = AppModel()
        let provider = try #require(ProviderID(rawValue: "nebula"))

        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))
        model.handle(.playerReady(deviceID: "device-1", name: "Nebula Player"))

        #expect(model.banner == "nebula local playback unavailable: region restricted")
    }

    @Test("transient banners do not discard provider-policy state")
    func transientBannerPreservesProviderPolicyState() throws {
        let model = AppModel()
        let provider = try #require(ProviderID(rawValue: "nebula"))

        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))
        model.handle(.rateLimited(retryAfterSecs: 5, scope: "browse", provider: provider))
        #expect(model.banner == "Rate limited — retrying in 5s")

        model.clearBanner()
        #expect(model.banner == "nebula local playback unavailable: region restricted")
    }

    @Test("player recovery restores policy beneath a transient failure")
    func playerRecoveryRestoresProviderPolicyBanner() throws {
        let model = AppModel()
        let provider = try #require(ProviderID(rawValue: "nebula"))

        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))
        model.handle(.playerFailed(reason: "session failed", restarts: 2))
        model.handle(.playerReady(deviceID: "device-1", name: "Nebula Player"))

        #expect(model.banner == "nebula local playback unavailable: region restricted")
    }

    @Test("exact recovery clears policy but stale clear cannot remove newer reason")
    func providerPolicyClearUsesExactIdentity() throws {
        let model = AppModel()
        let provider = try #require(ProviderID(rawValue: "nebula"))

        model.handle(.providerPolicy(provider: provider, reason: "old restriction"))
        model.handle(.providerPolicy(provider: provider, reason: "new restriction"))
        model.handle(.providerPolicyCleared(provider: provider, reason: "old restriction"))
        #expect(model.banner == "nebula local playback unavailable: new restriction")

        model.handle(.providerPolicyCleared(provider: provider, reason: "new restriction"))
        #expect(model.banner == nil)
    }

    @Test("dismissal survives identical seed and resets after recovery")
    func providerPolicyDismissalUsesExactIdentity() throws {
        let provider = try #require(ProviderID(rawValue: "nebula"))
        let model = AppModel()
        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))
        model.clearBanner()
        #expect(model.banner == nil)

        let same = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[{"provider":"nebula","reason":"region restricted"}]}"#.utf8))
        model.applyClientSeed(same)
        #expect(model.banner == nil)

        model.handle(.providerPolicyCleared(provider: provider, reason: "region restricted"))
        model.handle(.providerPolicy(provider: provider, reason: "region restricted"))
        #expect(model.banner == "nebula local playback unavailable: region restricted")
    }

    @Test("policy add newer than seed request is not overwritten")
    func providerPolicyAddWinsSeedRace() throws {
        let provider = try #require(ProviderID(rawValue: "nebula"))
        let model = AppModel()
        let revisionAtRequest = model.providerPolicyRevisionSnapshot
        model.handle(.providerPolicy(provider: provider, reason: "new restriction"))

        let stale = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[]}"#.utf8))
        model.applyClientSeed(
            stale, providerPolicyRevisionAtRequest: revisionAtRequest, seedGeneration: 1)

        #expect(model.banner == "nebula local playback unavailable: new restriction")
    }

    @Test("policy clear newer than seed request is not resurrected")
    func providerPolicyClearWinsSeedRace() throws {
        let provider = try #require(ProviderID(rawValue: "nebula"))
        let model = AppModel()
        model.handle(.providerPolicy(provider: provider, reason: "resolved restriction"))
        let revisionAtRequest = model.providerPolicyRevisionSnapshot
        model.handle(.providerPolicyCleared(
            provider: provider, reason: "resolved restriction"))

        let stale = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[{"provider":"nebula","reason":"resolved restriction"}]}"#.utf8))
        model.applyClientSeed(
            stale, providerPolicyRevisionAtRequest: revisionAtRequest, seedGeneration: 1)

        #expect(model.banner == nil)
    }

    @Test("overlapping seeds keep newer empty policy state in both completion orders")
    func overlappingSeedNewerEmptyWins() throws {
        let oldActive = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[{"provider":"nebula","reason":"old restriction"}]}"#.utf8))
        let newEmpty = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[]}"#.utf8))

        for newerCompletesFirst in [false, true] {
            let model = AppModel()
            let oldRequest = model.beginClientSeedRequest()
            let newRequest = model.beginClientSeedRequest()
            if newerCompletesFirst {
                model.applyClientSeed(
                    newEmpty,
                    providerPolicyRevisionAtRequest: newRequest.providerPolicyRevision,
                    seedGeneration: newRequest.generation)
                model.applyClientSeed(
                    oldActive,
                    providerPolicyRevisionAtRequest: oldRequest.providerPolicyRevision,
                    seedGeneration: oldRequest.generation)
            } else {
                model.applyClientSeed(
                    oldActive,
                    providerPolicyRevisionAtRequest: oldRequest.providerPolicyRevision,
                    seedGeneration: oldRequest.generation)
                model.applyClientSeed(
                    newEmpty,
                    providerPolicyRevisionAtRequest: newRequest.providerPolicyRevision,
                    seedGeneration: newRequest.generation)
            }
            #expect(model.banner == nil)
        }
    }

    @Test("overlapping seeds keep newer active policy state in both completion orders")
    func overlappingSeedNewerActiveWins() throws {
        let oldEmpty = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[]}"#.utf8))
        let newActive = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_policies":[{"provider":"nebula","reason":"new restriction"}]}"#.utf8))

        for newerCompletesFirst in [false, true] {
            let model = AppModel()
            let oldRequest = model.beginClientSeedRequest()
            let newRequest = model.beginClientSeedRequest()
            if newerCompletesFirst {
                model.applyClientSeed(
                    newActive,
                    providerPolicyRevisionAtRequest: newRequest.providerPolicyRevision,
                    seedGeneration: newRequest.generation)
                model.applyClientSeed(
                    oldEmpty,
                    providerPolicyRevisionAtRequest: oldRequest.providerPolicyRevision,
                    seedGeneration: oldRequest.generation)
            } else {
                model.applyClientSeed(
                    oldEmpty,
                    providerPolicyRevisionAtRequest: oldRequest.providerPolicyRevision,
                    seedGeneration: oldRequest.generation)
                model.applyClientSeed(
                    newActive,
                    providerPolicyRevisionAtRequest: newRequest.providerPolicyRevision,
                    seedGeneration: newRequest.generation)
            }
            #expect(model.banner == "nebula local playback unavailable: new restriction")
        }
    }

    @Test("player ready clears a transient player-failure banner")
    func playerReadyClearsPlayerFailureBanner() {
        let model = AppModel()

        model.handle(.playerFailed(reason: "session failed", restarts: 2))
        #expect(model.banner?.contains("session failed") == true)
        model.handle(.playerReady(deviceID: "device-1", name: "Recovered Player"))

        #expect(model.banner == nil)
    }

    @Test("capability catalog distinguishes legacy unknown, empty, and supported")
    func capabilityGates() throws {
        let legacy = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[]}"#.utf8))
        let model = AppModel()
        model.applyClientSeed(legacy)
        #expect(model.providerCatalog == nil)
        #expect(model.canPlay(uri: "spotify:track:1"))
        #expect(model.canQueue(uri: "spotify:track:1"))
        #expect(model.canListPlaylists)
        #expect(model.canReadPlaylistItems)
        #expect(model.canReadPlaylistItems(uri: "spotify:playlist:focus"))
        #expect(!model.canReadPlaylistItems(uri: "spotify:track:focus"))

        let empty = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"providers":[]}}"#.utf8))
        model.applyClientSeed(empty)
        #expect(model.providerCatalog?.providers.isEmpty == true)
        #expect(!model.canPlay(uri: "spotify:track:1"))
        #expect(!model.canQueue(uri: "spotify:track:1"))
        #expect(!model.canListDevices)

        let selective = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"spotify","providers":[{"id":"spotify","uri_scheme":"spotify","display_name":"Spotify","is_default":true,"capabilities":{"search":{"remote":true,"kinds":["track"]},"library":{"save_kinds":["track"]},"transport":{"play":true,"resume":true}}}]}}"#.utf8))
        model.applyClientSeed(selective)
        #expect(model.canPlay(uri: "spotify:track:1"))
        #expect(!model.canPlay(uri: "apple:track:1"))
        #expect(!model.canQueue(uri: "spotify:track:1"))
        #expect(model.canSave(uri: "spotify:track:1"))
        #expect(!model.canFollow(uri: "spotify:artist:1"))
        #expect(!model.canListPlaylists)
        #expect(!model.canReadLibrary(kind: .track))
        #expect(model.canSearch(source: .spotify, kinds: [.track]))
        #expect(!model.canSearch(source: .spotify, kinds: [.album]))
    }

    @Test("playlist item readability transitions after catalog seed and follows URI owner")
    func playlistItemReadTransitionUsesOwningProvider() throws {
        let model = AppModel()
        #expect(model.canReadPlaylistItems(uri: "quasar:playlist:focus"))

        let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"nebula","providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","is_default":true,"capabilities":{"playlists":{"item_read":false}}},{"id":"quasar","uri_scheme":"quasar","display_name":"Quasar Audio","capabilities":{"playlists":{"item_read":true}}}]}}"#.utf8))
        model.applyClientSeed(seed)

        #expect(!model.canReadPlaylistItems)
        #expect(!model.canReadPlaylistItems(uri: "nebula:playlist:focus"))
        #expect(model.canReadPlaylistItems(uri: "quasar:playlist:focus"))
        #expect(!model.canReadPlaylistItems(uri: "quasar:track:focus"))
        #expect(!model.canReadPlaylistItems(uri: "unknown:playlist:focus"))
    }

    @Test("global search follows the capable catalog default and explicit selection")
    func globalSearchUsesCatalogProviders() throws {
        let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"nebula","providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","is_default":true,"capabilities":{"search":{"remote":true,"kinds":["track"]}}},{"id":"quasar","uri_scheme":"quasar","display_name":"Quasar Audio","capabilities":{"search":{"remote":true,"kinds":["track"]}}}]}}"#.utf8))
        let model = AppModel()
        model.applyClientSeed(seed)
        let nebula = try #require(ProviderID(rawValue: "nebula"))
        let quasar = try #require(ProviderID(rawValue: "quasar"))

        #expect(model.search.selectedSource == .remote(nebula))
        #expect(model.search.sourceOptions.map(\.source) == [
            .remote(nebula), .remote(quasar), .local,
        ])

        let defaultRequest = try #require(model.search.searchRequest(query: "focus"))
        guard case .search(
            _, _, let defaultSource, _, let defaultProvider, _, _
        ) = defaultRequest else {
            Issue.record("expected default-provider search request")
            return
        }
        #expect(defaultSource == .remote(nebula))
        #expect(defaultProvider == nebula)

        model.search.setSource(.remote(quasar))
        let selectedRequest = try #require(model.search.searchRequest(query: "focus"))
        guard case .search(
            _, _, let selectedSource, _, let selectedProvider, _, _
        ) = selectedRequest else {
            Issue.record("expected selected-provider search request")
            return
        }
        #expect(selectedSource == .remote(quasar))
        #expect(selectedProvider == quasar)

        model.search.setSource(.local)
        let localRequest = try #require(model.search.searchRequest(query: "focus"))
        guard case .search(_, _, let localSource, _, let localProvider, _, _) = localRequest else {
            Issue.record("expected local search request")
            return
        }
        #expect(localSource == .local)
        #expect(localProvider == nil)
    }

    @Test("global search kinds follow the selected provider and prune stale filters")
    func globalSearchKindsFollowSelectedProvider() throws {
        let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"nebula","providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","is_default":true,"capabilities":{"search":{"remote":true,"kinds":["track"]}}},{"id":"quasar","uri_scheme":"quasar","display_name":"Quasar Audio","capabilities":{"search":{"remote":true,"kinds":["album","show"]}}}]}}"#.utf8))
        let model = AppModel()
        model.applyClientSeed(seed)
        let quasar = try #require(ProviderID(rawValue: "quasar"))

        #expect(model.search.filterableKinds == [.track])
        model.search.toggleFilter(.track)
        #expect(model.search.typeFilter == [.track])

        model.search.setSource(.remote(quasar))
        #expect(model.search.filterableKinds == [.album, .show])
        #expect(model.search.typeFilter.isEmpty)

        let request = try #require(model.search.searchRequest(query: "focus"))
        guard case .search(_, _, _, _, _, let kinds, _) = request else {
            Issue.record("expected provider search request")
            return
        }
        #expect(kinds == [.album, .show])
    }

    @Test("playlist resource URIs preserve canonical IDs and scope legacy IDs")
    func playlistResourceURIsAreProviderNeutral() throws {
        let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"nebula","providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","is_default":true,"capabilities":{}}]}}"#.utf8))
        let model = AppModel()
        model.applyClientSeed(seed)
        let legacy = try JSONDecoder().decode(Playlist.self, from: Data(
            #"{"id":"focus","name":"Focus","owner":"me","tracks_total":1,"image_url":null}"#.utf8))
        let canonical = try JSONDecoder().decode(Playlist.self, from: Data(
            #"{"id":"quasar:playlist:focus","name":"Focus","owner":"me","tracks_total":1,"image_url":null}"#.utf8))

        #expect(model.playlistResourceURI(for: legacy) == "nebula:playlist:focus")
        #expect(model.playlistResourceURI(for: canonical) == "quasar:playlist:focus")

        let legacyModel = AppModel()
        #expect(legacyModel.playlistResourceURI(for: legacy) == "spotify:playlist:focus")
        #expect(legacyModel.playlistResourceURI(for: canonical) == "quasar:playlist:focus")

        let emptyCatalog = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"providers":[]}}"#.utf8))
        let authoritativeEmptyModel = AppModel()
        authoritativeEmptyModel.applyClientSeed(emptyCatalog)
        #expect(authoritativeEmptyModel.playlistResourceURI(for: legacy) == nil)
    }

    @Test("global search without a provider catalog exposes only local search")
    func globalSearchWithoutCatalogIsProviderNeutral() {
        let model = AppModel()

        #expect(model.search.sourceOptions.map(\.source) == [.local])
        #expect(model.search.selectedSource == .local)
    }

    @Test("podcast catalog search routes through a non-Spotify default provider")
    func podcastSearchUsesCatalogDefault() throws {
        let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"default_provider":"nebula","providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","is_default":true,"capabilities":{"search":{"remote":true,"kinds":["show"]}}},{"id":"quasar","uri_scheme":"quasar","display_name":"Quasar Audio","capabilities":{"search":{"remote":true,"kinds":["show"]}}}]}}"#.utf8))
        let model = AppModel()
        model.applyClientSeed(seed)
        let nebula = try #require(ProviderID(rawValue: "nebula"))
        #expect(model.podcasts.selectedCatalogSource == .remote(nebula))
        #expect(model.podcasts.selectedCatalogLabel == "Nebula Music")
        let quasar = try #require(ProviderID(rawValue: "quasar"))
        #expect(model.podcasts.catalogSourceOptions.map(\.source) == [
            .remote(nebula), .remote(quasar),
        ])

        let selectedCatalogSource = try #require(model.podcasts.selectedCatalogSource)
        model.podcasts.setSource(selectedCatalogSource)
        let request = try #require(model.podcasts.catalogSearchRequest(query: "science"))
        guard case .search(
            let query, let scope, let source, let limit, let provider, let kinds, let sort
        ) = request else {
            Issue.record("expected catalog search request")
            return
        }
        #expect(query == "science")
        #expect(scope == .show)
        #expect(source == .remote(nebula))
        #expect(limit == 40)
        #expect(provider == nebula)
        #expect(kinds == nil)
        #expect(sort == nil)

        model.podcasts.setSource(.remote(quasar))
        let selectedRequest = try #require(
            model.podcasts.catalogSearchRequest(query: "history"))
        guard case .search(
            _, _, let selectedSource, _, let selectedProvider, _, _
        ) = selectedRequest else {
            Issue.record("expected selected-provider catalog search request")
            return
        }
        #expect(selectedSource == .remote(quasar))
        #expect(selectedProvider == quasar)

        model.podcasts.setSource(.hybrid)
        let hybridRequest = try #require(
            model.podcasts.catalogSearchRequest(query: "local and remote"))
        guard case .search(
            _, _, let hybridSource, _, let hybridProvider, _, _
        ) = hybridRequest else {
            Issue.record("expected hybrid catalog search request")
            return
        }
        #expect(hybridSource == .hybrid)
        #expect(hybridProvider == nil)
    }

    @Test("authoritative catalogs without a default expose no podcast catalog option")
    func podcastSearchRejectsMissingCatalogDefault() throws {
        let nebula = try #require(ProviderID(rawValue: "nebula"))
        for raw in [
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"providers":[]}}"#,
            #"{"playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},"queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"provider_catalog":{"providers":[{"id":"nebula","uri_scheme":"nebula","display_name":"Nebula Music","capabilities":{"search":{"remote":true,"kinds":["show"]}}}]}}"#,
        ] {
            let seed = try JSONDecoder().decode(ClientSeed.self, from: Data(raw.utf8))
            let model = AppModel()
            model.applyClientSeed(seed)

            #expect(model.podcasts.catalogSourceOptions.isEmpty)
            #expect(model.podcasts.selectedCatalogSource == nil)
            model.podcasts.setSource(.remote(nebula))
            #expect(model.podcasts.source == .local)
            if let _ = model.podcasts.catalogSearchRequest(query: "science") {
                Issue.record("authoritative no-default catalog must not produce a search request")
            }
        }
    }
}
