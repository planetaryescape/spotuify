import Foundation
import Darwin
import os

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
/// Identity tag for `handleClose` — set once right after the socket is
/// created, read from its own close callback. Weak so a replaced
/// socket can deallocate (a nil tag then never matches the live one).
private final class SocketRef: @unchecked Sendable {
    weak var value: IPCSocket?
}

public actor DaemonConnection {
    private var socket: IPCSocket?
    private var nextID: UInt64 = 1
    private var pending: [UInt64: CheckedContinuation<ResponseData, Error>] = [:]
    private var mutationRetryCache = MutationRetryCache()
    private let logger = Logger(subsystem: "com.bhekanik.spotuify", category: "ipc")

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
        let ref = SocketRef()
        let newSocket = IPCSocket(
            fd: fd,
            onFrame: { frame in cont.yield(frame) },
            onClose: { [weak self] error in
                guard let self else { return }
                // Identity-tagged: a STALE socket's close callback
                // (delivered async after teardown/reconnect) must not
                // tear down the connection that replaced it.
                let closing = ref.value
                Task { await self.handleClose(error, of: closing) }
            })
        ref.value = newSocket
        socket = newSocket
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

    private func handleClose(_ error: Error?, of closing: IPCSocket?) {
        // Only the CURRENT socket's close tears the connection down; a
        // stale callback from a replaced socket nilled the fresh one,
        // failed its pending requests, and sent the supervisor through
        // a phantom reconnect.
        guard let socket, socket === closing else { return }
        self.socket = nil
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
        timeout: Duration = .seconds(40)
    ) async throws -> ResponseData {
        let attempt = try mutationRetryCache.attempt(for: request)
        do {
            let response = try await self.request(attempt.prepared, timeout: timeout)
            mutationRetryCache.finish(attempt, uncertainOutcome: false)
            return response
        } catch {
            mutationRetryCache.finish(
                attempt,
                disposition: Self.mutationAttemptDisposition(after: error))
            throw error
        }
    }

    /// Only a lost transport response leaves the daemon's write outcome
    /// unknowable. Local setup/encoding failures prove the request was not
    /// sent; daemon errors prove it received and rejected the request.
    static func shouldRetainMutationAttempt(after error: Error) -> Bool {
        mutationAttemptDisposition(after: error) == .uncertain
    }

    static func mutationAttemptDisposition(after error: Error) -> MutationAttemptDisposition {
        if error is DaemonError {
            return .definitive
        }
        guard let connectionError = error as? DaemonConnectionError else { return .notSent }
        switch connectionError {
        case .timeout, .disconnected:
            return .uncertain
        case .socketPathTooLong, .connectFailed, .notConnected, .unexpectedResponse:
            return .notSent
        }
    }

    /// Send a prepared request. Retain and reuse `prepared` to retry a timed
    /// out mutation with the same daemon deduplication key; this method does
    /// not retry automatically.
    @discardableResult
    public func request(
        _ prepared: PreparedDaemonRequest,
        timeout: Duration = .seconds(40)
    ) async throws -> ResponseData {
        guard let socket else { throw DaemonConnectionError.notConnected }
        let id = nextID
        nextID &+= 1
        let payload = try Wire.encodeOutbound(OutboundMessage(id: id, prepared: prepared))
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
        let message: IpcMessage
        do {
            message = try Wire.decodeMessage(frame)
        } catch {
            handleUndecodableFrame(frame, error: error)
            return
        }
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

    /// A frame that fails to decode must not silently strand a pending request
    /// until its timeout. Recover the correlation id from the envelope; if it
    /// names a pending response, fail that continuation now. Log every dropped
    /// frame (responses and events) so silent state-stops are diagnosable.
    private func handleUndecodableFrame(_ frame: Data, error: Error) {
        let description = String(describing: error)
        guard let envelope = try? Wire.decodeFrameEnvelope(frame) else {
            logger.error("dropped undecodable IPC frame: \(description, privacy: .public)")
            return
        }
        if envelope.type == "Response", let cont = pending.removeValue(forKey: envelope.id) {
            logger.error(
                "request \(envelope.id) failed: undecodable response: \(description, privacy: .public)")
            cont.resume(throwing: DaemonConnectionError.unexpectedResponse(
                "response payload failed to decode"))
        } else {
            let kind = envelope.type ?? "unknown"
            logger.error(
                "dropped undecodable \(kind, privacy: .public) frame id=\(envelope.id): \(description, privacy: .public)")
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
