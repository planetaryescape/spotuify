import Foundation

/// Errors raised while framing/deframing the IPC byte stream.
public enum FrameError: Error, Equatable, Sendable {
    /// A frame header advertised a length above the 16 MB cap the daemon enforces.
    case oversized(UInt32)
}

/// Decodes the daemon's length-delimited wire format: each frame is a
/// **4-byte big-endian** `UInt32` length prefix followed by that many UTF-8
/// JSON bytes (tokio_util `LengthDelimitedCodec`, `length_field_length(4)`,
/// 16 MB max frame). Bytes arrive in arbitrary chunks, so this buffers and
/// yields whole frames only.
struct FrameDecoder {
    static let maxFrameLength: UInt32 = 16 * 1024 * 1024

    private var buffer: [UInt8] = []

    mutating func append(_ data: Data) {
        buffer.append(contentsOf: data)
    }

    /// Pop every complete frame currently buffered. Partial trailing bytes
    /// stay buffered for the next call. Throws on an oversized header.
    mutating func drain() throws -> [Data] {
        var frames: [Data] = []
        var pos = 0
        while buffer.count - pos >= 4 {
            let length =
                (UInt32(buffer[pos]) << 24) |
                (UInt32(buffer[pos + 1]) << 16) |
                (UInt32(buffer[pos + 2]) << 8) |
                UInt32(buffer[pos + 3])
            if length > Self.maxFrameLength {
                throw FrameError.oversized(length)
            }
            let total = 4 + Int(length)
            guard buffer.count - pos >= total else { break }
            frames.append(Data(buffer[(pos + 4)..<(pos + total)]))
            pos += total
        }
        if pos > 0 {
            buffer.removeFirst(pos)
        }
        return frames
    }
}

/// Prepends the 4-byte big-endian length prefix to an outbound payload.
enum FrameEncoder {
    static func encode(_ payload: Data) -> Data {
        var prefix = UInt32(payload.count).bigEndian
        var out = Data(bytes: &prefix, count: 4)
        out.append(payload)
        return out
    }
}
