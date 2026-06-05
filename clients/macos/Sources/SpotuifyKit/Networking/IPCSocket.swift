import Foundation
import Darwin

/// Owns a connected AF_UNIX stream socket and does all raw byte I/O on a
/// private serial queue. Reads are driven by a `DispatchSourceRead` (the fd
/// stays blocking; the source only fires when data is available, so a single
/// `read` per fire returns promptly). Complete frames are handed to `onFrame`
/// in order; teardown calls `onClose` exactly once.
///
/// `@unchecked Sendable`: every mutable field is touched only on `queue`.
final class IPCSocket: @unchecked Sendable {
    private let fd: Int32
    private let queue: DispatchQueue
    private var readSource: DispatchSourceRead?
    private var frameDecoder = FrameDecoder()
    private var closed = false

    private let onFrame: @Sendable (Data) -> Void
    private let onClose: @Sendable (Error?) -> Void

    init(
        fd: Int32,
        onFrame: @escaping @Sendable (Data) -> Void,
        onClose: @escaping @Sendable (Error?) -> Void
    ) {
        self.fd = fd
        self.onFrame = onFrame
        self.onClose = onClose
        self.queue = DispatchQueue(label: "com.bhekanik.spotuify.ipc.\(fd)")
        start()
    }

    private func start() {
        let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
        source.setEventHandler { [weak self] in self?.handleReadable() }
        let capturedFd = fd
        source.setCancelHandler { Darwin.close(capturedFd) }
        readSource = source
        source.resume()
    }

    private func handleReadable() {
        guard !closed else { return }
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        let bytesRead = buffer.withUnsafeMutableBytes { raw in
            Darwin.read(fd, raw.baseAddress, raw.count)
        }
        if bytesRead > 0 {
            frameDecoder.append(Data(buffer[0..<bytesRead]))
            do {
                for frame in try frameDecoder.drain() { onFrame(frame) }
            } catch {
                fail(error)
            }
        } else if bytesRead == 0 {
            fail(nil) // EOF — daemon closed
        } else {
            if errno == EINTR || errno == EAGAIN { return } // source refires
            fail(posixError())
        }
    }

    /// Enqueue a fully-framed payload for writing. Safe to call from any thread.
    func send(_ frame: Data) {
        queue.async { [weak self] in self?.writeAll(frame) }
    }

    private func writeAll(_ data: Data) {
        guard !closed else { return }
        data.withUnsafeBytes { raw in
            guard var pointer = raw.baseAddress else { return }
            var remaining = raw.count
            while remaining > 0 {
                let written = Darwin.write(fd, pointer, remaining)
                if written > 0 {
                    pointer = pointer.advanced(by: written)
                    remaining -= written
                } else if written < 0 && errno == EINTR {
                    continue
                } else {
                    fail(posixError())
                    return
                }
            }
        }
    }

    /// Close the socket and notify once. Idempotent.
    func close() {
        queue.async { [weak self] in self?.fail(nil) }
    }

    private func fail(_ error: Error?) {
        guard !closed else { return }
        closed = true
        readSource?.cancel() // cancel handler closes the fd
        onClose(error)
    }

    private func posixError() -> Error {
        DaemonConnectionError.connectFailed(String(cString: strerror(errno)))
    }
}
