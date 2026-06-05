import Foundation
import Testing
@testable import SpotuifyKit

@Suite("Frame codec")
struct FrameCodecTests {
    @Test("round-trips a single frame")
    func roundTrip() throws {
        var decoder = FrameDecoder()
        let payload = Data("hello".utf8)
        let framed = FrameEncoder.encode(payload)
        #expect(framed.count == 4 + payload.count)
        // Big-endian length prefix
        #expect(Array(framed.prefix(4)) == [0, 0, 0, 5])
        decoder.append(framed)
        #expect(try decoder.drain() == [payload])
    }

    @Test("buffers a partial header until the rest arrives")
    func partialHeader() throws {
        var decoder = FrameDecoder()
        let framed = FrameEncoder.encode(Data("abcd".utf8))
        decoder.append(framed.prefix(3))
        #expect(try decoder.drain().isEmpty)
        decoder.append(framed.suffix(from: framed.index(framed.startIndex, offsetBy: 3)))
        #expect(try decoder.drain() == [Data("abcd".utf8)])
    }

    @Test("splits two frames concatenated in one chunk")
    func twoFrames() throws {
        var decoder = FrameDecoder()
        var buffer = Data()
        buffer.append(FrameEncoder.encode(Data("one".utf8)))
        buffer.append(FrameEncoder.encode(Data("two".utf8)))
        decoder.append(buffer)
        #expect(try decoder.drain() == [Data("one".utf8), Data("two".utf8)])
    }

    @Test("rejects a frame above the 16 MB cap")
    func oversized() {
        var decoder = FrameDecoder()
        var length = UInt32(17 * 1024 * 1024).bigEndian
        var data = Data(bytes: &length, count: 4)
        data.append(contentsOf: [0, 0, 0, 0])
        decoder.append(data)
        #expect(throws: FrameError.self) { var d = decoder; _ = try d.drain() }
    }
}
