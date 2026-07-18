import Foundation
import Testing
@testable import SpotuifyKit

@Suite("Wire decoding")
struct WireDecodingTests {
    private func decode(_ json: String) throws -> IpcMessage {
        try Wire.decodeMessage(Data(json.utf8))
    }

    @Test("decodes a playback-changed event with an embedded snapshot")
    func playbackChangedEvent() throws {
        let json = """
        {"id":0,"payload":{"type":"Event","event":"playback-changed","action":"optimistic-pause",
        "playback":{"item":{"id":"abc","uri":"spotify:track:abc","name":"Song","subtitle":"Artist",
        "context":"playlist","duration_ms":180000,"image_url":"https://i.scdn.co/image/x","kind":"track",
        "explicit":true,"is_playable":true},
        "device":{"id":"d1","name":"Mac","type":"computer","is_active":true,"is_restricted":false,
        "volume_percent":75,"supports_volume":true},
        "is_playing":false,"progress_ms":45000,"shuffle":false,"repeat":"off",
        "sampled_at_ms":1700000000000,"source":"player-event"}}}
        """
        let message = try decode(json)
        guard case .event(.playbackChanged(let action, let playback)) = message.payload else {
            Issue.record("expected playbackChanged, got \(message.payload)"); return
        }
        #expect(action == "optimistic-pause")
        #expect(playback?.isPlaying == false)
        #expect(playback?.progressMs == 45000)
        #expect(playback?.item?.name == "Song")
        #expect(playback?.item?.kind == .track)
        #expect(playback?.device?.volumePercent == 75)
        #expect(playback?.sampledAtMs == 1_700_000_000_000)
    }

    @Test("decodes an Ok playback response")
    func okPlaybackResponse() throws {
        let json = """
        {"id":1,"payload":{"type":"Response","Ok":{"data":{"kind":"playback",
        "playback":{"is_playing":true,"progress_ms":1000,"shuffle":true,"repeat":"track"}}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.playback(let playback))) = message.payload else {
            Issue.record("expected Ok playback, got \(message.payload)"); return
        }
        #expect(message.id == 1)
        #expect(playback.isPlaying)
        #expect(playback.repeatMode == "track")
        #expect(playback.item == nil)        // absent optional
        #expect(playback.sampledAtMs == nil) // absent optional
    }

    @Test("decodes a cover-art cache response")
    func coverArtResponse() throws {
        let json = """
        {"id":9,"payload":{"type":"Response","Ok":{"data":{"kind":"cover-art",
        "path":"/Users/me/Library/Caches/spotuify/covers/abc.jpg",
        "cache_hit":true,"bytes":12345,"fetched_at_ms":1700000000000}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.coverArt(let path, let cacheHit, let bytes, let fetchedAtMs))) = message.payload else {
            Issue.record("expected cover-art, got \(message.payload)"); return
        }
        #expect(path.hasSuffix("/abc.jpg"))
        #expect(cacheHit)
        #expect(bytes == 12_345)
        #expect(fetchedAtMs == 1_700_000_000_000)
    }

    @Test("decodes a client-seed response and ignores the viz field")
    func clientSeedResponse() throws {
        let json = """
        {"id":2,"payload":{"type":"Response","Ok":{"data":{"kind":"client-seed",
        "playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},
        "queue":{"currently_playing":null,"items":[],"session_active":true,"as_of_ms":123},
        "devices":[{"id":"d1","name":"Mac","type":"computer","is_active":true,"is_restricted":false,
        "volume_percent":50,"supports_volume":true}],
        "recent":[],"viz":{"active":"none","configured":"auto"}}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.clientSeed(let seed))) = message.payload else {
            Issue.record("expected client-seed, got \(message.payload)"); return
        }
        #expect(seed.devices.count == 1)
        #expect(seed.queue.isSessionActive == true)
        #expect(seed.queue.asOfMs == 123)
        #expect(seed.recent.isEmpty)
        #expect(seed.providerCatalog == nil)
        #expect(seed.preferences == nil)
    }

    @Test("decodes provider catalog defaults and client preferences")
    func providerCatalogResponse() throws {
        let json = """
        {"id":7,"payload":{"type":"Response","Ok":{"data":{"kind":"provider-list",
        "default_provider":"spotify","providers":[{"id":"spotify","uri_scheme":"spotify",
        "display_name":"Spotify","is_default":true,"capabilities":{"search":{"remote":true,
        "kinds":["track"],"max_page_size":50},"transport":{"play":true}}}]}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.providerList(let defaultProvider, let providers))) = message.payload
        else {
            Issue.record("expected provider-list, got \(message.payload)"); return
        }
        #expect(defaultProvider == .spotify)
        #expect(providers.first?.displayName == "Spotify")
        #expect(providers.first?.capabilities.search.remote == true)
        #expect(providers.first?.capabilities.search.maxPageSize == 50)
        #expect(providers.first?.capabilities.library.saveKinds.isEmpty == true)
        #expect(providers.first?.capabilities.transport?.play == true)
        #expect(providers.first?.capabilities.transport?.pause == false)
    }

    @Test("client seed distinguishes absent from explicit empty catalog")
    func clientSeedExplicitEmptyCatalog() throws {
        let json = """
        {"id":8,"payload":{"type":"Response","Ok":{"data":{"kind":"client-seed",
        "playback":{"is_playing":false,"progress_ms":0,"shuffle":false,"repeat":"off"},
        "queue":{"currently_playing":null,"items":[]},"devices":[],"recent":[],"viz":{},
        "provider_catalog":{"providers":[]},"preferences":{"viz_color_scheme":"provider"}}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.clientSeed(let seed))) = message.payload else {
            Issue.record("expected client-seed, got \(message.payload)"); return
        }
        #expect(seed.providerCatalog?.providers.isEmpty == true)
        #expect(seed.preferences?.visualizationColorScheme == "provider")
    }

    @Test("target-resolved preserves exact null and audio outputs decode")
    func providerUtilityResponses() throws {
        let target = try decode(
            #"{"id":9,"payload":{"type":"Response","Ok":{"data":{"kind":"target-resolved","target":null}}}}"#)
        guard case .response(.ok(.targetResolved(let resolved))) = target.payload else {
            Issue.record("expected target-resolved, got \(target.payload)"); return
        }
        #expect(resolved == nil)

        let audio = try decode(
            #"{"id":10,"payload":{"type":"Response","Ok":{"data":{"kind":"audio-outputs","outputs":["Mac Speakers","DAC"],"selected":"DAC"}}}}"#)
        guard case .response(.ok(.audioOutputs(let outputs, let selected))) = audio.payload else {
            Issue.record("expected audio-outputs, got \(audio.payload)"); return
        }
        #expect(outputs == ["Mac Speakers", "DAC"])
        #expect(selected == "DAC")
    }

    @Test("provider-tagged search and sync wire decodes")
    func providerTaggedSearchAndSync() throws {
        let started = try decode(
            #"{"id":11,"payload":{"type":"Response","Ok":{"data":{"kind":"search-started","query":"x","version":2,"provider":"spotify"}}}}"#)
        guard case .response(.ok(.searchStarted(let query, let version, let provider))) = started.payload
        else {
            Issue.record("expected search-started, got \(started.payload)"); return
        }
        #expect(query == "x")
        #expect(version == 2)
        #expect(provider == .spotify)

        let sync = try decode(
            #"{"id":12,"payload":{"type":"Event","event":"sync-finished","summary":{"target":"all","playback_snapshots":1,"devices":2,"playlists":3,"playlist_items":4,"recent_items":5,"library_items":6,"media_items":7,"provider_outcomes":[{"provider":"spotify","status":"partial","error":"limited"}]}}}"#)
        guard case .event(.syncFinished(let summary)) = sync.payload else {
            Issue.record("expected sync-finished, got \(sync.payload)"); return
        }
        #expect(summary.status == .succeeded)
        #expect(summary.queueSnapshots == 0)
        #expect(summary.providerOutcomes.first?.provider == .spotify)
        #expect(summary.providerOutcomes.first?.status == .partial)
    }

    @Test("decodes generic and legacy provider policy events")
    func providerPolicyEvents() throws {
        let generic = try decode(
            #"{"id":13,"payload":{"type":"Event","event":"provider-policy","provider":"nebula","reason":"region restricted"}}"#)
        guard case .event(.providerPolicy(let provider, let reason)) = generic.payload else {
            Issue.record("expected provider-policy, got \(generic.payload)"); return
        }
        #expect(provider.rawValue == "nebula")
        #expect(reason == "region restricted")

        let cleared = try decode(
            #"{"id":15,"payload":{"type":"Event","event":"provider-policy-cleared","provider":"nebula","reason":"region restricted"}}"#)
        guard case .event(.providerPolicyCleared(let provider, let reason)) = cleared.payload else {
            Issue.record("expected provider-policy-cleared, got \(cleared.payload)"); return
        }
        #expect(provider.rawValue == "nebula")
        #expect(reason == "region restricted")

        let legacy = try decode(
            #"{"id":14,"payload":{"type":"Event","event":"premium-required"}}"#)
        guard case .event(.premiumRequired) = legacy.payload else {
            Issue.record("expected legacy premium-required, got \(legacy.payload)"); return
        }
    }

    @Test("decodes an Error response and flags auth-revoked")
    func errorResponse() throws {
        let json = """
        {"id":3,"payload":{"type":"Response","Error":{"message":"refresh token revoked",
        "kind":"auth_revoked","code":"auth_revoked","retryable":false,
        "provider":"spotify","detail":"refresh grant rejected"}}}
        """
        let message = try decode(json)
        guard case .response(.error(let err)) = message.payload else {
            Issue.record("expected error, got \(message.payload)"); return
        }
        #expect(err.message == "refresh token revoked")
        #expect(err.isAuthRevoked)
        #expect(err.provider == .spotify)
        #expect(err.detail == "refresh grant rejected")
    }

    @Test("decodes daemon-owned auth session state")
    func authSessionResponse() throws {
        let json = #"{"id":4,"payload":{"type":"Response","Ok":{"data":{"kind":"auth-session","session":{"session_id":"018f47d2-9e2a-7000-8000-000000000001","provider":"spotify","method":"dev_app","state":{"state":"waiting","authorization_url":"https://accounts.example/authorize","redirect_uri":"http://127.0.0.1/callback"},"created_at_ms":1,"expires_at_ms":2}}}}}"#
        let message = try decode(json)
        guard case .response(.ok(.authSession(let session))) = message.payload else {
            Issue.record("expected auth-session, got \(message.payload)"); return
        }
        #expect(session.provider == .spotify)
        guard case .waiting(let authorizationURL, _, _) = session.state else {
            Issue.record("expected waiting state"); return
        }
        #expect(authorizationURL.contains("authorize"))
    }

    @Test("decodes secret-free auth status")
    func authStatusResponse() throws {
        let json = #"{"id":5,"payload":{"type":"Response","Ok":{"data":{"kind":"auth-status","status":{"provider":"spotify","strategy":"spotify_oauth","auth_required":false,"auth_revoked":false,"credentials":[{"kind":"dev_app","present":true,"expires_at_ms":123,"scopes":["streaming"],"missing_scopes":[]}]}}}}}"#
        let message = try decode(json)
        guard case .response(.ok(.authStatus(let status))) = message.payload else {
            Issue.record("expected auth-status, got \(message.payload)"); return
        }
        #expect(status.strategy == .spotifyOauth)
        #expect(status.credentials.first?.kind == .devApp)
        #expect(status.credentials.first?.expiresAtMs == 123)
    }

    @Test("decodes auth logout receipt")
    func authLogoutResponse() throws {
        let json = #"{"id":6,"payload":{"type":"Response","Ok":{"data":{"kind":"auth-logout","result":{"provider":"spotify","removed_dev_app":true,"removed_first_party":true,"removed_librespot":true,"auth_required":true}}}}}"#
        let message = try decode(json)
        guard case .response(.ok(.authLogout(let result))) = message.payload else {
            Issue.record("expected auth-logout, got \(message.payload)"); return
        }
        #expect(result.removedLibrespot)
        #expect(result.authRequired)
    }

    @Test("decodes a streaming search-page event")
    func searchPageEvent() throws {
        let json = """
        {"id":0,"payload":{"type":"Event","event":"search-page","query":"daft punk","kind":"track",
        "offset":0,"version":7,"items":[{"uri":"spotify:track:1","name":"One More Time",
        "subtitle":"Daft Punk","context":"","duration_ms":320000,"kind":"track"}]}}
        """
        let message = try decode(json)
        guard case .event(.searchPage(
            let query, let kind, let offset, let version, let items, let provider)) = message.payload
        else {
            Issue.record("expected search-page, got \(message.payload)"); return
        }
        #expect(query == "daft punk")
        #expect(kind == .track)
        #expect(offset == 0)
        #expect(version == 7)
        #expect(items.first?.name == "One More Time")
        #expect(provider == nil) // frozen legacy event still decodes
    }

    @Test("decodes provider-tagged search and library events")
    func providerTaggedEvents() throws {
        let search = try decode(
            #"{"id":0,"payload":{"type":"Event","event":"search-updated","query":"x","count":1,"provider":"spotify"}}"#)
        guard case .event(.searchUpdated(let query, let count, let searchProvider)) = search.payload
        else {
            Issue.record("expected search-updated, got \(search.payload)"); return
        }
        #expect(query == "x")
        #expect(count == 1)
        #expect(searchProvider == .spotify)

        let library = try decode(
            #"{"id":0,"payload":{"type":"Event","event":"library-changed","action":"save","uris":["spotify:track:1"],"provider":"spotify"}}"#)
        guard case .event(.libraryChanged(_, let uris, let libraryProvider)) = library.payload
        else {
            Issue.record("expected library-changed, got \(library.payload)"); return
        }
        #expect(uris == ["spotify:track:1"])
        #expect(libraryProvider == .spotify)
    }

    @Test("unknown event kinds fall back to .unknown")
    func unknownEvent() throws {
        let json = #"{"id":0,"payload":{"type":"Event","event":"future-provider-event"}}"#
        let message = try decode(json)
        guard case .event(.unknown(let event)) = message.payload else {
            Issue.record("expected unknown event, got \(message.payload)"); return
        }
        #expect(event == "future-provider-event")
    }

    @Test("unknown response kinds fall back to .unknown")
    func unknownResponseKind() throws {
        let json = #"{"id":4,"payload":{"type":"Response","Ok":{"data":{"kind":"cache-status","status":{"rows":1}}}}}"#
        let message = try decode(json)
        guard case .response(.ok(.unknown(let kind))) = message.payload else {
            Issue.record("expected unknown kind, got \(message.payload)"); return
        }
        #expect(kind == "cache-status")
    }

    @Test("decodes a daemon-status response with protocol version")
    func daemonStatusResponse() throws {
        let json = #"{"id":5,"payload":{"type":"Response","Ok":{"data":{"kind":"daemon-status","status":{"running":true,"socket_path":"/x","socket_exists":true,"socket_reachable":true,"stale_socket":false,"protocol_version":2,"daemon_version":"0.1.41"}}}}}"#
        let message = try decode(json)
        guard case .response(.ok(.daemonStatus(let status))) = message.payload else {
            Issue.record("expected daemon-status, got \(message.payload)"); return
        }
        #expect(status.protocolVersion == 2)
        #expect(status.daemonVersion == "0.1.41")
        #expect(status.running)
    }

    @Test("decodes a track's new metadata fields (album, added_at, episode resume)")
    func decodesEnrichedMediaItem() throws {
        let json = """
        {"id":1,"payload":{"type":"Response","Ok":{"data":{"kind":"media-items","items":[
        {"uri":"spotify:track:1","name":"Song","subtitle":"Artist","context":"Album","duration_ms":1000,
         "kind":"track","album":"Greatest Hits","added_at_ms":1700000000000},
        {"uri":"spotify:episode:e1","name":"Ep","subtitle":"Show","context":"Show","duration_ms":3600000,
         "kind":"episode","fully_played":true,"resume_position_ms":120000,"release_date":"2024-03-01"}
        ]}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.mediaItems(let items))) = message.payload else {
            Issue.record("expected media-items, got \(message.payload)"); return
        }
        #expect(items[0].album == "Greatest Hits")
        #expect(items[0].addedAtMs == 1_700_000_000_000)
        #expect(items[0].albumLabel == "Greatest Hits")
        #expect(items[1].isFullyPlayed)
        #expect(items[1].resumePositionMs == 120_000)
        #expect(items[1].releaseDate == "2024-03-01")
    }

    @Test("decodes a paged saved-tracks response with total + offset")
    func decodesSavedTracksPage() throws {
        let json = """
        {"id":1,"payload":{"type":"Response","Ok":{"data":{"kind":"saved-tracks-page","items":[
        {"uri":"spotify:track:1","name":"Song","subtitle":"Artist","context":"Album","duration_ms":1000,
         "kind":"track","added_at_ms":1700000000000}
        ],"total":4200,"offset":50}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.savedTracksPage(let items, let total, let offset))) = message.payload
        else {
            Issue.record("expected saved-tracks-page, got \(message.payload)"); return
        }
        #expect(items.count == 1)
        #expect(items[0].uri == "spotify:track:1")
        #expect(total == 4200)
        #expect(offset == 50)
    }

    @Test("decodes a reminders list response")
    func remindersResponse() throws {
        let json = """
        {"id":1,"payload":{"type":"Response","Ok":{"data":{"kind":"reminders","reminders":[
        {"id":"r1","media_uri":"spotify:album:a","media_kind":"album","name":"Album","subtitle":"Artist",
         "anchor_at_ms":1700000000000,"recurrence":"weekly","tz":"UTC","next_due_at_ms":1700600000000,
         "state":"active","created_at_ms":1699999999000}]}}}}
        """
        let message = try decode(json)
        guard case .response(.ok(.reminders(let reminders))) = message.payload else {
            Issue.record("expected reminders, got \(message.payload)"); return
        }
        #expect(reminders.count == 1)
        #expect(reminders[0].recurrence == .weekly)
        #expect(reminders[0].mediaKind == .album)
        #expect(reminders[0].state == .active)
    }

    @Test("decodes a reminder-due event with an embedded notification")
    func reminderDueEvent() throws {
        let json = """
        {"id":0,"payload":{"type":"Event","event":"reminder-due","notification":
        {"id":"n1","reminder_id":"r1","media_uri":"spotify:track:t","media_kind":"track","name":"Song",
         "subtitle":"Artist","due_at_ms":1700000000000,"fired_at_ms":1700000000500,"state":"unseen",
         "message":"listen!"}}}
        """
        let message = try decode(json)
        guard case .event(.reminderDue(let notification)) = message.payload else {
            Issue.record("expected reminder-due, got \(message.payload)"); return
        }
        #expect(notification.id == "n1")
        #expect(notification.reminderID == "r1")
        #expect(notification.state == .unseen)
        #expect(notification.isOpen)
        #expect(notification.message == "listen!")
    }

    @Test("decodes a spectrum-frame event")
    func spectrumFrameEvent() throws {
        let json = """
        {"id":0,"payload":{"type":"Event","event":"spectrum-frame",
        "bands":[0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,1.0,0.5,0.25],"peak":1.0,"timestamp_ms":999}}
        """
        let message = try decode(json)
        guard case .event(.spectrumFrame(let bands, let peak, let ts)) = message.payload else {
            Issue.record("expected spectrum-frame, got \(message.payload)"); return
        }
        #expect(bands.count == 12)
        #expect(peak == 1.0)
        #expect(ts == 999)
    }
}
