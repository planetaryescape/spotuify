import Foundation
import Observation

/// Backs the Settings window's visual config editor. Reads the whole config via
/// `spotuify config show --format json` and writes changes with
/// `spotuify config set <key> <value>` + `reload` (debounced per key). The
/// daemon owns the config file; this is a view + a thin CLI driver.
@MainActor
@Observable
public final class ConfigStore {
    /// Flat `key -> value` map mirroring the CLI's settable keys. `client_secret`
    /// arrives as `<redacted>` and is only overwritten when the user types a new
    /// value (see `isRedactedSecret`).
    public private(set) var values: [String: String] = [:]
    public private(set) var loading = false
    public private(set) var saving = false
    public private(set) var errorMessage: String?
    /// Local audio output device names (for the Playback picker), lazily loaded.
    public private(set) var audioOutputs: [String] = []

    private weak var model: AppModel?
    private var pending: [String: Task<Void, Never>] = [:]

    public init() {}
    func connect(_ model: AppModel) { self.model = model }

    public static let redactedSecret = "<redacted>"
    public func isRedactedSecret(_ key: String) -> Bool {
        key == "client_secret" && string(key) == Self.redactedSecret
    }

    // MARK: Reads

    public func string(_ key: String) -> String { values[key] ?? "" }
    public func bool(_ key: String) -> Bool {
        let v = string(key).lowercased()
        return v == "true" || v == "1"
    }
    public func int(_ key: String) -> Int { Int(string(key)) ?? 0 }

    // MARK: Load

    public func load() async {
        loading = true
        defer { loading = false }
        do {
            let json = try await CLIRunner.run(["config", "show", "--format", "json"])
            guard let data = json.data(using: .utf8),
                  let map = try? JSONDecoder().decode([String: String].self, from: data)
            else {
                errorMessage = "Could not parse config"
                return
            }
            values = map
            errorMessage = nil
        } catch {
            errorMessage = (error as? CLIRunner.CLIError)?.message ?? "Failed to read config"
        }
    }

    /// Populate `audioOutputs` from `spotuify audio-outputs --format json`.
    public func loadAudioOutputs() async {
        guard audioOutputs.isEmpty else { return }
        guard let json = try? await CLIRunner.run(["audio-outputs", "--format", "json"]),
              let data = json.data(using: .utf8) else { return }
        // The command emits an array of objects with a `name` field, or strings.
        if let names = try? JSONDecoder().decode([String].self, from: data) {
            audioOutputs = names
        } else if let objs = try? JSONDecoder().decode([[String: String]].self, from: data) {
            audioOutputs = objs.compactMap { $0["name"] }
        }
    }

    // MARK: Write (debounced)

    /// Persist `key=value`: optimistic local update, then `config set` + `reload`
    /// after a short debounce so rapid edits coalesce into one write.
    public func set(_ key: String, _ value: String) {
        if values[key] == value { return }
        values[key] = value
        pending[key]?.cancel()
        pending[key] = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(450))
            guard let self, !Task.isCancelled else { return }
            self.saving = true
            defer { self.saving = false }
            do {
                _ = try await CLIRunner.run(["config", "set", key, value])
                // `player.*` changes (backend, audio output device, bitrate, …)
                // only take effect when the player backend is rebuilt, so issue
                // a `reconnect`. Other keys just need the daemon to re-read.
                let apply = key.hasPrefix("player.") ? "reconnect" : "reload"
                _ = try? await CLIRunner.run([apply], timeout: 30)
                self.errorMessage = nil
            } catch {
                self.errorMessage =
                    (error as? CLIRunner.CLIError)?.message ?? "Failed to save \(key)"
            }
        }
    }

    public func setBool(_ key: String, _ value: Bool) { set(key, value ? "true" : "false") }
    public func setInt(_ key: String, _ value: Int) { set(key, String(value)) }
}
