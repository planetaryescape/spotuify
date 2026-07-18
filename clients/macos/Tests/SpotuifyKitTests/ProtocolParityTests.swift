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
