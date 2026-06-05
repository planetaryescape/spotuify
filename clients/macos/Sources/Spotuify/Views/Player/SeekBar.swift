import SwiftUI

/// A draggable progress/seek bar. While dragging it shows a local value and
/// commits the seek (a single daemon command) only on release — staying true
/// to the daemon-owned-state rule.
struct SeekBar: View {
    /// 0...1 current progress (daemon-authoritative).
    let progress: Double
    /// Called with a 0...1 fraction when the user commits a seek.
    let onSeek: (Double) -> Void

    var height: CGFloat = 6

    @State private var dragFraction: Double?
    @State private var hovering = false

    private var shownFraction: Double { dragFraction ?? progress }

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            ZStack(alignment: .leading) {
                Capsule().fill(.primary.opacity(0.15))
                Capsule().fill(.tint)
                    .frame(width: max(0, min(1, shownFraction)) * width)
                Circle()
                    .fill(.white)
                    .frame(width: height + 6, height: height + 6)
                    .shadow(radius: 1, y: 0.5)
                    .offset(x: max(0, min(1, shownFraction)) * width - (height + 6) / 2)
                    .opacity(hovering || dragFraction != nil ? 1 : 0)
            }
            .frame(height: height)
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
        .frame(height: height + 6)
        .animation(.easeOut(duration: 0.12), value: hovering)
    }
}
