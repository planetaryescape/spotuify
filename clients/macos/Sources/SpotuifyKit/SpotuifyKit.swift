import Foundation

/// Namespace for the spotuify macOS client library. The kit holds all the
/// testable, non-UI logic (IPC framing, wire models, stores, system bridges)
/// so it can be unit-tested without launching the app.
public enum SpotuifyKit {
    /// IPC protocol version this client speaks. Mirrors
    /// `spotuify_protocol::IPC_PROTOCOL_VERSION`.
    public static let ipcProtocolVersion = 1
}
