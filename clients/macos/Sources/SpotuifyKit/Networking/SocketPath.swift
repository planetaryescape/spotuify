import Foundation

/// Resolves the daemon's Unix-socket path with the same rules as
/// `spotuify_protocol::paths` on macOS:
/// `~/Library/Application Support/<instance>/daemon.sock`, where `<instance>`
/// defaults to `spotuify` (the installed binary). `SPOTUIFY_SOCKET` overrides
/// the whole path; `SPOTUIFY_RUNTIME_DIR` overrides the directory;
/// `SPOTUIFY_INSTANCE` overrides the instance name.
public enum SocketPath {
    public static let defaultInstance = "spotuify"
    public static let devInstance = "spotuify-dev"

    public static func resolve(instance: String? = nil) -> String {
        let env = ProcessInfo.processInfo.environment
        if let explicit = env["SPOTUIFY_SOCKET"], !explicit.isEmpty {
            return explicit
        }
        if let runtimeDir = env["SPOTUIFY_RUNTIME_DIR"], !runtimeDir.isEmpty {
            return (runtimeDir as NSString).appendingPathComponent("daemon.sock")
        }
        let resolved = instance
            ?? env["SPOTUIFY_INSTANCE"].flatMap { $0.isEmpty ? nil : $0 }
            ?? defaultInstance
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return "\(home)/Library/Application Support/\(resolved)/daemon.sock"
    }
}
