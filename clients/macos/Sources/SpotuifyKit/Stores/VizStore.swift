import Foundation
import Observation

/// Holds the latest audio spectrum frame from the daemon's visualizer. Frames
/// arrive at ~30 Hz; this store is kept separate so its high-frequency updates
/// only invalidate the visualizer view, not the whole UI.
@MainActor
@Observable
public final class VizStore {
    public static let bandCount = 12

    public private(set) var bands: [Float] = Array(repeating: 0, count: bandCount)
    public private(set) var peak: Float = 0

    public init() {}

    func apply(bands: [Float], peak: Float) {
        // Defensive: the protocol promises 12 bands but tolerate drift.
        if bands.count == Self.bandCount {
            self.bands = bands
        } else {
            var padded = bands
            padded += Array(repeating: 0, count: max(0, Self.bandCount - bands.count))
            self.bands = Array(padded.prefix(Self.bandCount))
        }
        self.peak = peak
    }

    func reset() {
        bands = Array(repeating: 0, count: Self.bandCount)
        peak = 0
    }
}
