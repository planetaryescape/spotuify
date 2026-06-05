import Foundation
import Testing
@testable import SpotuifyKit

/// Integration tests that talk to a REAL running daemon. Auto-enabled only
/// when the socket is reachable, so a daemon-less CI run skips them. These
/// stay read-only (no playback mutation) per the project's smoke-test rules.
@Suite("Live daemon IPC", .enabled(if: DaemonLauncher.probe(SocketPath.resolve())))
struct LiveDaemonTests {
    @Test("handshake: ping → pong, subscribe, client-seed, playback-get")
    func handshake() async throws {
        let path = SocketPath.resolve()
        let connection = DaemonConnection()
        try await connection.connect(to: path)
        defer { Task { await connection.close() } }

        let pong = try await connection.request(.ping, timeout: .seconds(5))
        guard case .pong = pong else {
            Issue.record("expected pong, got \(pong)"); return
        }

        try await connection.subscribeEvents()

        let seedResponse = try await connection.request(.clientSeed, timeout: .seconds(10))
        guard case .clientSeed(let seed) = seedResponse else {
            Issue.record("expected client-seed, got \(seedResponse)"); return
        }
        print("[live] seed: devices=\(seed.devices.count) recent=\(seed.recent.count) "
            + "playing=\(seed.playback.isPlaying) track=\(seed.playback.item?.name ?? "<none>") "
            + "queue=\(seed.queue.items.count)")

        let playbackResponse = try await connection.request(.playbackGet, timeout: .seconds(10))
        switch playbackResponse {
        case .playback(let playback):
            print("[live] playback-get: track=\(playback.item?.name ?? "<none>") "
                + "device=\(playback.device?.name ?? "<none>") source=\(playback.source ?? "?")")
        case .unknown(let kind):
            Issue.record("playback-get returned unknown kind \(kind)")
        default:
            Issue.record("expected playback, got \(playbackResponse)")
        }
    }

    @Test("search returns real catalog results")
    func search() async throws {
        let connection = DaemonConnection()
        try await connection.connect(to: SocketPath.resolve())
        defer { Task { await connection.close() } }

        let response = try await connection.request(
            .search(query: "luther vandross", scope: .all, source: .spotify, limit: 20),
            timeout: .seconds(20))
        guard case .searchResults(let items) = response else {
            Issue.record("expected search-results, got \(response)"); return
        }
        let kinds = Set(items.map(\.kind))
        print("[live] search 'luther vandross': \(items.count) items, kinds=\(kinds.map(\.rawValue).sorted())")
        #expect(!items.isEmpty)
        #expect(items.contains { $0.kind == .track })
    }

    @Test("playlists-list and library-list return real data")
    func libraryAndPlaylists() async throws {
        let connection = DaemonConnection()
        try await connection.connect(to: SocketPath.resolve())
        defer { Task { await connection.close() } }

        if case .playlists(let playlists) = try await connection.request(.playlistsList, timeout: .seconds(20)) {
            print("[live] playlists: \(playlists.count) (first: \(playlists.first?.name ?? "<none>"))")
            #expect(!playlists.isEmpty)
        } else {
            Issue.record("expected playlists response")
        }

        if case .mediaItems(let items) = try await connection.request(.libraryList(limit: 50), timeout: .seconds(20)) {
            print("[live] library: \(items.count) saved items")
        } else {
            Issue.record("expected media-items for library-list")
        }
    }

    @Test("lyrics-get round-trips for the current track")
    func lyrics() async throws {
        let connection = DaemonConnection()
        try await connection.connect(to: SocketPath.resolve())
        defer { Task { await connection.close() } }

        // Resolve a track to ask lyrics for: prefer current playback.
        var trackURI: String?
        if case .playback(let playback) = try await connection.request(.playbackGet, timeout: .seconds(10)) {
            trackURI = playback.item?.uri
        }
        if trackURI == nil, case .queue(let queue) = try await connection.request(.queueGet, timeout: .seconds(10)) {
            trackURI = queue.currentlyPlaying?.uri
        }
        guard let trackURI else {
            print("[live] lyrics: no current track to query — skipping assertion")
            return
        }

        let response = try await connection.request(
            .lyricsGet(trackURI: trackURI, forceRefresh: false), timeout: .seconds(25))
        guard case .lyrics(let synced, let offset) = response else {
            Issue.record("expected lyrics response, got \(response)"); return
        }
        print("[live] lyrics for \(trackURI): "
            + (synced.map { "\($0.lines.count) lines, provider=\($0.provider), synced=\($0.synced), offset=\(offset)" }
               ?? "none available"))
    }
}
