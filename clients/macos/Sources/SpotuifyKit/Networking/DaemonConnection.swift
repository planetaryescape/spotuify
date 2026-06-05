import Foundation
import Darwin

/// Speaks the daemon IPC protocol over a single Unix socket.
///
/// - `request(_:)` correlates responses by the monotonically increasing `id`
///   and enforces a timeout so a lost reply never leaks a continuation.
/// - `events` is a long-lived ordered stream of `DaemonEvent`s (subscribe via
///   `subscribeEvents()` once).
/// - `states` reports connect/disconnect transitions so a supervisor
///   (AppModel) can reconnect and re-seed.
///
/// Raw byte I/O lives in `IPCSocket`; this actor owns only protocol state.
public actor DaemonConnection {
    private var socket: IPCSocket?
    private var nextID: UInt64 = 1
    private var pending: [UInt64: CheckedContinuation<ResponseData, Error>] = [:]

    public nonisolated let events: AsyncStream<DaemonEvent>
    private let eventContinuation: AsyncStream<DaemonEvent>.Continuation

    private nonisolated let inbound: AsyncStream<Data>
    private nonisolated let inboundContinuation: AsyncStream<Data>.Continuation

    /// Resumed when the active socket closes, so a supervisor can await a
    /// disconnect and reconnect.
    private var closeWaiters: [CheckedContinuation<Void, Never>] = []

    public init() {
        (events, eventContinuation) = AsyncStream.makeStream()
        (inbound, inboundContinuation) = AsyncStream.makeStream()
        Task { await self.consumeInbound() }
    }

    // MARK: Connection lifecycle

    /// Open the socket. Tears down any previous connection first.
    public func connect(to path: String) throws {
        teardown()
        let fd = try Self.openSocket(path: path)
        let cont = inboundContinuation
        socket = IPCSocket(
            fd: fd,
            onFrame: { frame in cont.yield(frame) },
            onClose: { [weak self] error in
                guard let self else { return }
                Task { await self.handleClose(error) }
            })
    }

    public func close() {
        teardown()
    }

    /// Suspends until the current socket closes (or returns immediately if
    /// there is no active socket).
    public func waitUntilClosed() async {
        guard socket != nil else { return }
        await withCheckedContinuation { closeWaiters.append($0) }
    }

    private func teardown() {
        let hadSocket = socket != nil
        socket?.close()
        socket = nil
        failAllPending()
        if hadSocket { resumeCloseWaiters() }
    }

    private func handleClose(_ error: Error?) {
        guard socket != nil else { return } // already torn down
        socket = nil
        failAllPending()
        resumeCloseWaiters()
    }

    private func failAllPending() {
        let failure = DaemonConnectionError.disconnected
        for (_, cont) in pending { cont.resume(throwing: failure) }
        pending.removeAll()
    }

    private func resumeCloseWaiters() {
        let waiters = closeWaiters
        closeWaiters.removeAll()
        for waiter in waiters { waiter.resume() }
    }

    // MARK: Requests

    @discardableResult
    public func request(
        _ request: DaemonRequest,
        timeout: Duration = .seconds(30)
    ) async throws -> ResponseData {
        guard let socket else { throw DaemonConnectionError.notConnected }
        let id = nextID
        nextID &+= 1
        let payload = try Wire.encodeOutbound(OutboundMessage(id: id, request: request))
        let frame = FrameEncoder.encode(payload)

        let timeoutTask = Task { [weak self] in
            try? await Task.sleep(for: timeout)
            await self?.expire(id)
        }
        defer { timeoutTask.cancel() }

        return try await withCheckedThrowingContinuation { continuation in
            pending[id] = continuation
            socket.send(frame)
        }
    }

    /// Subscribe this connection to the daemon's event broadcast (once).
    public func subscribeEvents() async throws {
        _ = try await request(.subscribeEvents)
    }

    private func expire(_ id: UInt64) {
        if let cont = pending.removeValue(forKey: id) {
            cont.resume(throwing: DaemonConnectionError.timeout)
        }
    }

    // MARK: Inbound routing (single ordered consumer)

    private func consumeInbound() async {
        for await frame in inbound {
            ingest(frame)
        }
    }

    private func ingest(_ frame: Data) {
        guard let message = try? Wire.decodeMessage(frame) else { return }
        switch message.payload {
        case .response(let response):
            guard let cont = pending.removeValue(forKey: message.id) else { return }
            switch response {
            case .ok(let data):
                cont.resume(returning: data)
            case .error(let error):
                cont.resume(throwing: error)
            }
        case .event(let event):
            eventContinuation.yield(event)
        case .other:
            break
        }
    }

    // MARK: Socket open

    static func openSocket(path: String) throws -> Int32 {
        let capacity = MemoryLayout.size(ofValue: sockaddr_un().sun_path)
        guard path.utf8.count < capacity else {
            throw DaemonConnectionError.socketPathTooLong(path)
        }
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw DaemonConnectionError.connectFailed("socket(): \(String(cString: strerror(errno)))")
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cString in
            withUnsafeMutablePointer(to: &addr.sun_path) { tuplePtr in
                tuplePtr.withMemoryRebound(to: CChar.self, capacity: capacity) { dest in
                    _ = strncpy(dest, cString, capacity - 1)
                }
            }
        }

        let size = socklen_t(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &addr) { addrPtr in
            addrPtr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                Darwin.connect(fd, sockPtr, size)
            }
        }
        guard result == 0 else {
            let saved = errno
            Darwin.close(fd)
            throw DaemonConnectionError.connectFailed(
                "connect(\(path)): \(String(cString: strerror(saved)))")
        }
        return fd
    }
}
