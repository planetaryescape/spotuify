import SwiftUI
import SpotuifyKit

/// A 12-band spectrum visualizer driven by the daemon's `spectrum-frame`
/// events. Bars settle to flat when playback is paused.
struct VisualizerView: View {
    @Environment(AppModel.self) private var model
    var barCount: Int { VizStore.bandCount }

    var body: some View {
        let bands = model.viz.bands
        let live = model.player.isPlaying
        GeometryReader { geo in
            let spacing: CGFloat = 4
            let barWidth = (geo.size.width - spacing * CGFloat(barCount - 1)) / CGFloat(barCount)
            HStack(alignment: .bottom, spacing: spacing) {
                ForEach(0..<barCount, id: \.self) { index in
                    let value = live ? CGFloat(min(1, max(0.02, bands[safe: index] ?? 0))) : 0.02
                    Capsule()
                        .fill(.tint)
                        .frame(width: barWidth, height: max(2, value * geo.size.height))
                        .animation(.easeOut(duration: 0.08), value: value)
                }
            }
            .frame(maxHeight: .infinity, alignment: .bottom)
        }
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
