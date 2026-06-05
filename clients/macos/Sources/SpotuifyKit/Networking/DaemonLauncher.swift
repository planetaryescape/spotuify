import Foundation
import Darwin

/// Ensures the `spotuify` daemon is running, spawning it if needed. GUI apps
/// launched from Finder get a minimal PATH, so we probe known Homebrew/Cargo
/// locations explicitly (and honor `SPOTUIFY_BIN`).
public enum DaemonLauncher {
    public static func resolveBinary() -> String? {
        let env = ProcessInfo.processInfo.environment
        if let explicit = env["SPOTUIFY_BIN"], !explicit.isEmpty,
           FileManager.default.isExecutableFile(atPath: explicit) {
            return explicit
        }
        var candidates = [
            "/opt/homebrew/bin/spotuify",
            "/usr/local/bin/spotuify",
        ]
        if let home = env["HOME"] {
            candidates.append("\(home)/.cargo/bin/spotuify")
        }
        if let pathVar = env["PATH"] {
            for dir in pathVar.split(separator: ":") {
                candidates.append("\(dir)/spotuify")
            }
        }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    /// Returns true once the socket accepts a connection. If it doesn't
    /// initially, spawns `spotuify daemon start` and polls until `timeout`.
    @discardableResult
    public static func ensureRunning(
        socketPath: String,
        timeout: Duration = .seconds(8)
    ) async -> Bool {
        if probe(socketPath) { return true }
        guard let binary = resolveBinary() else { return false }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["daemon", "start"]
        process.standardOutput = nil
        process.standardError = nil
        do { try process.run() } catch { return false }

        let deadline = ContinuousClock.now.advanced(by: timeout)
        while ContinuousClock.now < deadline {
            if probe(socketPath) { return true }
            try? await Task.sleep(for: .milliseconds(200))
        }
        return probe(socketPath)
    }

    /// Cheap liveness check: try to connect, then immediately close.
    public static func probe(_ path: String) -> Bool {
        guard let fd = try? DaemonConnection.openSocket(path: path) else { return false }
        Darwin.close(fd)
        return true
    }
}
