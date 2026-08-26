import Foundation
import Testing

@testable import SpotuifyKit

/// Anchors `Bundle(for:)` so the test can load its bundled fixture.
private final class FixtureAnchor {}

/// Enforces that the macOS `DaemonRequest` roster stays in lockstep with
/// the Rust `Request` roster. The fixture `request-kinds.json` is the
/// shared contract: the Rust test `rust_roster_matches_macos_fixture`
/// keeps it equal to `Request::all_kind_labels()`, and this test keeps
/// the Swift enum equal to the fixture — both directions, so a request
/// added on either side fails until the other catches up.
@Suite("Protocol parity")
struct ProtocolParityTests {
    private func fixtureCommands() throws -> Set<String> {
        let bundle = Bundle(for: FixtureAnchor.self)
        let url = try #require(
            bundle.url(forResource: "request-kinds", withExtension: "json"),
            "request-kinds.json missing from the test bundle; regenerate the Xcode project so Tests/SpotuifyKitTests/Fixtures is bundled"
        )
        let data = try Data(contentsOf: url)
        let labels = try JSONDecoder().decode([String].self, from: data)
        return Set(labels)
    }

    /// One entry per Rust event kind: the kind, and a minimal valid frame of it
    /// serialised back to `Data`. The frames are generated from the Rust
    /// `sample_frames()`, so the probes can't drift from what the daemon sends.
    private struct EventKindFixture {
        let kind: String
        let sample: Data
    }

    private func eventKindFixtures() throws -> [EventKindFixture] {
        let bundle = Bundle(for: FixtureAnchor.self)
        let url = try #require(
            bundle.url(forResource: "event-kinds", withExtension: "json"),
            "event-kinds.json missing from the test bundle; regenerate the Xcode project so Tests/SpotuifyKitTests/Fixtures is bundled"
        )
        let entries = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [[String: Any]]
        return try #require(entries).map { entry in
            EventKindFixture(
                kind: try #require(entry["kind"] as? String),
                sample: try JSONSerialization.data(withJSONObject: try #require(entry["sample"]))
            )
        }
    }

    /// Decodes the kind's real sample frame. Anything other than a decoded,
    /// non-`.unknown` case is a miss: a throw means the case exists but can't
    /// read a frame the daemon sends, which is just as broken as no case at all.
    private func decodeFailure(_ fixture: EventKindFixture) -> String? {
        do {
            let event = try JSONDecoder().decode(DaemonEvent.self, from: fixture.sample)
            if case .unknown = event { return "\(fixture.kind): fell through to .unknown" }
            return nil
        } catch {
            return "\(fixture.kind): threw \(error)"
        }
    }

    private func providerPolicyFixture() throws -> [IpcMessage] {
        let bundle = Bundle(for: FixtureAnchor.self)
        let url = try #require(
            bundle.url(forResource: "provider-policy-events", withExtension: "json"),
            "provider-policy-events.json missing from the test bundle"
        )
        let values = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [Any]
        return try #require(values).map { value in
            let data = try JSONSerialization.data(withJSONObject: value)
            return try Wire.decodeMessage(data)
        }
    }

    @Test("every Rust request kind has a DaemonRequest case")
    func swiftCoversRustRoster() throws {
        let fixture = try fixtureCommands()
        let swift = Set(DaemonRequest.allSamples.map(\.commandName))
        let missing = fixture.subtracting(swift)
        #expect(
            missing.isEmpty,
            "DaemonRequest is missing cases for Rust request kinds: \(missing.sorted())"
        )
    }

    @Test("DaemonRequest emits no command the Rust roster lacks")
    func rustRosterCoversSwift() throws {
        let fixture = try fixtureCommands()
        let swift = Set(DaemonRequest.allSamples.map(\.commandName))
        let extra = swift.subtracting(fixture)
        #expect(
            extra.isEmpty,
            "DaemonRequest emits commands absent from the Rust roster: \(extra.sorted())"
        )
    }

    @Test("every Rust event kind decodes into a real DaemonEvent case")
    func swiftCoversRustEventRoster() throws {
        let fixtures = try eventKindFixtures()
        #expect(fixtures.count == DaemonEvent.handledEventTags.count)
        let failures = fixtures.compactMap(decodeFailure)
        #expect(
            failures.isEmpty,
            "DaemonEvent cannot decode Rust event kinds: \(failures.sorted())"
        )

        let kinds = Set(fixtures.map(\.kind))
        #expect(
            kinds.subtracting(DaemonEvent.handledEventTags).isEmpty,
            "handledEventTags is missing Rust event kinds: \(kinds.subtracting(DaemonEvent.handledEventTags).sorted())"
        )
    }

    @Test("DaemonEvent handles no event kind the Rust roster lacks")
    func rustEventRosterCoversSwift() throws {
        let kinds = Set(try eventKindFixtures().map(\.kind))
        let extra = DaemonEvent.handledEventTags.subtracting(kinds)
        #expect(
            extra.isEmpty,
            "DaemonEvent handles event kinds absent from the Rust roster: \(extra.sorted())"
        )
    }

    @Test("allSamples has one entry per command (no duplicates)")
    func samplesAreUnique() {
        let commands = DaemonRequest.allSamples.map(\.commandName)
        #expect(
            commands.count == Set(commands).count,
            "allSamples has duplicate commands: \(commands.sorted())"
        )
        #expect(!commands.contains(""), "a sample failed to encode a cmd string")
    }

    @Test("generic provider-policy and released premium-required fixtures decode")
    func providerPolicyCompatibilityFixtures() throws {
        let messages = try providerPolicyFixture()
        #expect(messages.count == 2)
        guard case .event(.providerPolicy(let provider, let reason)) = messages[0].payload else {
            Issue.record("expected provider-policy fixture"); return
        }
        #expect(provider.rawValue == "nebula")
        #expect(reason == "region restricted")
        guard case .event(.premiumRequired) = messages[1].payload else {
            Issue.record("expected legacy premium-required fixture"); return
        }
    }
}
