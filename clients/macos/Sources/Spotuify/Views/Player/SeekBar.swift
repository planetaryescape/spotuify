import SwiftUI

/// A draggable progress/seek bar. While dragging it shows a local value and
/// commits the seek (a single daemon command) only on release — staying true
/// to the daemon-owned-state rule.
struct SeekBar: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// 0...1 current progress (daemon-authoritative).
    let progress: Double
    /// Total duration used for VoiceOver's time value and adjustment step.
    let durationMs: UInt64
    /// Called with a 0...1 fraction when the user commits a seek.
    let onSeek: (Double) -> Void

    var height: CGFloat = 6

    @State private var dragFraction: Double?
    @State private var hovering = false

    private var shownFraction: Double { dragFraction ?? progress }

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            // Apple-Music-style affordance: the bar thickens on hover/drag so
            // it's easier to grab, and the scrubber knob fades + scales in.
            let active = hovering || dragFraction != nil
            let barHeight = active ? height + 4 : height
            let knob = barHeight + 8
            ZStack(alignment: .leading) {
                Capsule().fill(.primary.opacity(0.15)).frame(height: barHeight)
                Capsule().fill(.tint)
                    .frame(width: max(0, min(1, shownFraction)) * width, height: barHeight)
                Circle()
                    .fill(.white)
                    .frame(width: knob, height: knob)
                    .shadow(radius: 2, y: 0.5)
                    .offset(x: max(0, min(1, shownFraction)) * width - knob / 2)
                    .opacity(active ? 1 : 0)
                    .scaleEffect(active ? 1 : 0.5)
            }
            .frame(height: max(barHeight, height), alignment: .center)
            .frame(maxHeight: .infinity, alignment: .center)
            .contentShape(Rectangle().inset(by: -8))
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        dragFraction = min(1, max(0, value.location.x / width))
                    }
                    .onEnded { value in
                        let fraction = min(1, max(0, value.location.x / width))
                        onSeek(fraction)
                        dragFraction = nil
                    }
            )
            .onHover { hovering = $0 }
        }
        .frame(height: height + 10)
        .animation(reduceMotion ? nil : .spring(response: 0.28, dampingFraction: 0.72), value: hovering)
        .animation(reduceMotion ? nil : .spring(response: 0.28, dampingFraction: 0.72), value: dragFraction != nil)
        .accessibilityElement()
        .accessibilityLabel("Playback position")
        .accessibilityValue(accessibilityValue)
        .accessibilityAdjustableAction { direction in
            let step = durationMs > 0 ? 5_000 / Double(durationMs) : 0.05
            let delta = direction == .increment ? step : -step
            onSeek(min(1, max(0, shownFraction + delta)))
        }
    }

    private var accessibilityValue: String {
        let currentMs = UInt64(max(0, min(1, shownFraction)) * Double(durationMs))
        return "\(Theme.timeString(currentMs)) / \(Theme.timeString(durationMs))"
    }
}
