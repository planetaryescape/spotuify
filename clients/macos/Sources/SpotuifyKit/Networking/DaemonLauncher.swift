import Foundation
import Darwin

/// Ensures the `spotuify` daemon is running, spawning it if needed. GUI apps
/// launched from Finder get a minimal PATH, so we probe known Homebrew/Cargo
/// locations explicitly (and honor `SPOTUIFY_BIN`).
public enum DaemonLauncher {
    /// The `spotuify` daemon+CLI binary bundled inside the .app (placed at
    /// Contents/Resources/spotuify by the DMG build), if present + executable.
    /// This makes the app self-contained — no Homebrew/Cargo required.
    public static func bundledBinaryPath() -> String? {
        guard let url = Bundle.main.url(forResource: "spotuify", withExtension: nil) else {
            return nil
        }
        return FileManager.default.isExecutableFile(atPath: url.path) ? url.path : nil
    }

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
            candidates.append("\(home)/.local/bin/spotuify")
            candidates.append("\(home)/.cargo/bin/spotuify")
        }
        if let pathVar = env["PATH"] {
            for dir in pathVar.split(separator: ":") {
                candidates.append("\(dir)/spotuify")
            }
        }
        if let found = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) {
            return found
        }
        // Last resort: the binary bundled in the app itself.
        return bundledBinaryPath()
    }

    /// Install the bundled `spotuify` binary to `~/.local/bin/spotuify` so the
    /// daemon+CLI are available on the user's PATH (the DMG is the whole backend).
    /// No-op when no bundled binary, when a system install (Homebrew /
    /// /usr/local / cargo) already provides the CLI — `~/.local/bin`
    /// precedes Homebrew on many PATHs, so installing alongside one
    /// would SHADOW it and resurrect the duplicate-install problem on
    /// every app launch — or when a current copy is already installed.
    @discardableResult
    public static func installBundledCLIIfNeeded() -> Bool {
        guard let bundled = bundledBinaryPath(),
              let home = ProcessInfo.processInfo.environment["HOME"] else { return false }
        let fm = FileManager.default
        let binDir = "\(home)/.local/bin"
        let dest = "\(binDir)/spotuify"
        let systemInstalls = [
            "/opt/homebrew/bin/spotuify",
            "/usr/local/bin/spotuify",
            "\(home)/.cargo/bin/spotuify",
        ]
        if systemInstalls.contains(where: { fm.isExecutableFile(atPath: $0) }) {
            // A system install owns the CLI. Also REMOVE any copy this
            // app previously dropped into ~/.local/bin: it precedes
            // Homebrew on many PATHs and a stale copy from an older
            // launch would shadow the real install forever.
            if fm.fileExists(atPath: dest) {
                try? fm.removeItem(atPath: dest)
            }
            return false
        }
        // Already installed + identical bytes → current, skip. (The old
        // size-equality heuristic could mistake a different build of
        // similar size for current.)
        if fm.contentsEqual(atPath: dest, andPath: bundled) {
            return true
        }
        do {
            try fm.createDirectory(atPath: binDir, withIntermediateDirectories: true)
            if fm.fileExists(atPath: dest) { try fm.removeItem(atPath: dest) }
            try fm.copyItem(atPath: bundled, toPath: dest)
            try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: dest)
            return true
        } catch {
            return false
        }
    }

    /// How long after an intentional `daemon stop` we refuse to relaunch.
    /// Long enough that a manual stop sticks and a `daemon restart` (stop then
    /// start, ~1-2s) doesn't race a relaunch; short enough that the menubar
    /// app still resumes keeping the daemon alive afterward.
    static let intentionalStopGrace: TimeInterval = 30

    /// True when `daemon stop` recently wrote its intentional-stop sentinel
    /// (beside the socket) and we are still inside the grace window — i.e. the
    /// user (or a `daemon restart`) just stopped the daemon on purpose and we
    /// should NOT relaunch it. `daemon start` removes the sentinel, so a
    /// genuine crash (no sentinel) still relaunches normally.
    static func recentIntentionalStop(socketPath: String) -> Bool {
        let dir = (socketPath as NSString).deletingLastPathComponent
        let sentinel = (dir as NSString).appendingPathComponent("intentional-stop")
        guard let raw = try? String(contentsOfFile: sentinel, encoding: .utf8),
              let stoppedAt = TimeInterval(raw.trimmingCharacters(in: .whitespacesAndNewlines))
        else { return false }
        let age = Date().timeIntervalSince1970 - stoppedAt
        return age >= 0 && age < intentionalStopGrace
    }

    /// Returns true once the socket accepts a connection. If it doesn't
    /// initially, spawns `spotuify daemon start` and polls until `timeout`.
    @discardableResult
    public static func ensureRunning(
        socketPath: String,
        timeout: Duration = .seconds(8)
    ) async -> Bool {
        if probe(socketPath) { return true }
        // Respect a deliberate `daemon stop`/`restart`: don't fight the user by
        // immediately relaunching a daemon they just stopped. Falls through to
        // normal relaunch once the grace window passes or `daemon start` clears
        // the sentinel.
        if recentIntentionalStop(socketPath: socketPath) { return false }
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
