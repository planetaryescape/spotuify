import Foundation

/// Drives the local `spotuify` install for the macOS daemon gate: starting the
/// daemon and (best-effort) upgrading it via Homebrew, then restarting it.
///
/// The CLI is the source of truth — these just invoke it. A first-class
/// `spotuify update` self-updater is the planned proper fix; this is the GUI
/// stopgap (Homebrew installs only).
public enum DaemonControl {
    public struct CommandResult: Sendable {
        public let ok: Bool
        public let output: String
    }

    /// The Homebrew upgrade + restart commands, for the Terminal fallback.
    public static let brewUpdateCommands = [
        "brew upgrade planetaryescape/spotuify/spotuify",
        "spotuify daemon restart",
    ]

    /// Start (or confirm) the daemon. Thin wrapper over the launcher.
    public static func startDaemon(socketPath: String) async -> Bool {
        await DaemonLauncher.ensureRunning(socketPath: socketPath)
    }

    /// `brew upgrade` the tap formula, then restart the daemon. Best-effort:
    /// returns combined output so the caller can fall back to Terminal on failure.
    public static func updateViaBrew(socketPath: String) async -> CommandResult {
        guard let brew = resolveBrew() else {
            return CommandResult(ok: false, output: "Homebrew not found in /opt/homebrew or /usr/local.")
        }
        let upgrade = await run(brew, ["upgrade", "planetaryescape/spotuify/spotuify"], timeout: 300)
        guard upgrade.ok else { return upgrade }

        guard let binary = DaemonLauncher.resolveBinary() else {
            return CommandResult(ok: false, output: upgrade.output + "\nUpgraded, but couldn't locate spotuify to restart.")
        }
        let restart = await run(binary, ["daemon", "restart"], timeout: 30)
        let reachable = await DaemonLauncher.ensureRunning(socketPath: socketPath)
        return CommandResult(ok: restart.ok && reachable, output: upgrade.output + "\n" + restart.output)
    }

    static func resolveBrew() -> String? {
        ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            .first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    /// Run a process to completion off the main thread, capturing combined
    /// stdout+stderr, with a hard timeout.
    static func run(_ path: String, _ args: [String], timeout seconds: Double) async -> CommandResult {
        // Box lets the timeout watchdog reach the process without capturing a
        // non-Sendable `Process` across queues.
        final class Box: @unchecked Sendable { let process = Process() }
        let box = Box()

        return await withCheckedContinuation { (continuation: CheckedContinuation<CommandResult, Never>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = box.process
                process.executableURL = URL(fileURLWithPath: path)
                process.arguments = args
                let pipe = Pipe()
                process.standardOutput = pipe
                process.standardError = pipe

                // GUI apps inherit a minimal PATH; give brew somewhere to look.
                var env = ProcessInfo.processInfo.environment
                let extra = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
                env["PATH"] = env["PATH"].map { "\(extra):\($0)" } ?? extra
                process.environment = env

                do {
                    try process.run()
                } catch {
                    continuation.resume(returning: CommandResult(
                        ok: false, output: "Failed to launch: \(error.localizedDescription)"))
                    return
                }

                let watchdog = DispatchWorkItem {
                    if box.process.isRunning { box.process.terminate() }
                }
                DispatchQueue.global().asyncAfter(deadline: .now() + seconds, execute: watchdog)

                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                watchdog.cancel()

                continuation.resume(returning: CommandResult(
                    ok: process.terminationStatus == 0,
                    output: String(decoding: data, as: UTF8.self)))
            }
        }
    }
}
