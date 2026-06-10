import Foundation
import Testing
@testable import SpotuifyKit

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
