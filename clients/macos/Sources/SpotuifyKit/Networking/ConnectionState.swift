import Foundation

/// High-level connection status, surfaced to the UI for banners/spinners.
public enum ConnectionState: Sendable, Equatable {
    case idle
    case connecting
    case ready
    case reconnecting(attempt: Int)
    case failed(String)
}

/// Errors raised by the IPC client layer.
public enum DaemonConnectionError: Error, Sendable, Equatable {
    case socketPathTooLong(String)
    case connectFailed(String)
    case notConnected
    case timeout
    case disconnected
    case unexpectedResponse(String)
}
