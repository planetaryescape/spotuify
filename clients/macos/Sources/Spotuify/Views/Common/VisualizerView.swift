import SwiftUI
import SpotuifyKit

/// Render styles for the spectrum visualizer.
enum VizStyle: String, CaseIterable, Identifiable {
    case bars, circular, wave
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .bars: "waveform"
        case .circular: "circle.dotted"
        case .wave: "wave.3.right"
        }
    }
}

/// A spectrum visualizer driven by the daemon's `spectrum-frame` events, in one
/// of several styles. Bars settle to flat when playback is paused. The palette
/// accent drives a gradient + glow so it reads as part of the album-themed UI
/// rather than a generic meter.
struct VisualizerView: View {
    @Environment(AppModel.self) private var model
    var style: VizStyle = .bars
    /// Concrete tint (Canvas styles can't read `.tint`); defaults to the accent.
    var tint: Color = .accentColor

    private var barCount: Int { VizStore.bandCount }

    var body: some View {
        #if DEBUG
        // `-SpotuifyDemoSpectrum YES` renders a synthetic spectrum so the
        // styles can be checked against a daemon that has no PCM to analyse
        // (the fake provider) without starting real playback.
        if UserDefaults.standard.bool(forKey: "SpotuifyDemoSpectrum") {
            TimelineView(.animation(minimumInterval: 1 / 20)) { context in
                styled(Self.demoValues(at: context.date.timeIntervalSinceReferenceDate, count: barCount))
            }
        } else {
            styled(liveValues)
        }
        #else
        styled(liveValues)
        #endif
    }

    private var liveValues: [Double] {
        let bands = model.viz.bands
        let live = model.player.isPlaying
        return (0..<barCount).map { index in
            live ? min(1.0, max(0.02, Double(bands[safe: index] ?? 0))) : 0.02
        }
    }

    @ViewBuilder
    private func styled(_ values: [Double]) -> some View {
        switch style {
        case .bars: BarsViz(values: values, tint: tint)
        case .circular: CircularViz(values: values, tint: tint)
        case .wave: WaveViz(values: values, tint: tint)
        }
    }

    #if DEBUG
    /// A music-shaped spectrum: heavier lows, a moving mid bump, jitter on top.
    static func demoValues(at t: TimeInterval, count: Int) -> [Double] {
        (0..<count).map { i in
            let x = Double(i) / Double(max(count - 1, 1))
            let lows = 0.75 * (1 - x) * (0.7 + 0.3 * sin(t * 7.1))
            let mids = 0.5 * exp(-pow((x - (0.45 + 0.2 * sin(t * 0.9))) * 4, 2))
            let jitter = 0.12 * (0.5 + 0.5 * sin(t * 13 + Double(i) * 1.7))
            return min(1, max(0.02, lows + mids + jitter))
        }
    }
    #endif
}

/// Mirrored gradient bars with rounded caps, a soft accent glow, and a glassy
/// highlight along the top — springy so levels feel alive.
private struct BarsViz: View {
    let values: [Double]
    let tint: Color

    var body: some View {
        GeometryReader { geo in
            let spacing: CGFloat = 6
            let count = max(values.count, 1)
            let barWidth = min(
                max(3, (geo.size.width - spacing * CGFloat(count - 1)) / CGFloat(count)), 22)
            // Height is driven by the band level only (NOT bar width) so a wider
            // window can't make the bars grow taller.
            HStack(alignment: .center, spacing: spacing) {
                ForEach(Array(values.enumerated()), id: \.offset) { _, value in
                    Capsule()
                        .fill(
                            LinearGradient(
                                colors: [tint, tint.opacity(0.45)],
                                startPoint: .top, endPoint: .bottom)
                        )
                        .overlay(alignment: .top) {
                            // Glassy sheen on the cap.
                            Capsule()
                                .fill(.white.opacity(0.35))
                                .frame(height: barWidth)
                                .blur(radius: 1)
                        }
                        .frame(width: barWidth, height: max(5, CGFloat(value) * geo.size.height))
                        .animation(.spring(response: 0.34, dampingFraction: 0.62), value: value)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
            .shadow(color: tint.opacity(0.45), radius: 12)
            .shadow(color: tint.opacity(0.25), radius: 3)
        }
    }
}

/// Radiating gradient spokes with rounded tips, a glowing core orb, and a faint
/// guide ring — a polished take on the radial meter.
private struct CircularViz: View {
    let values: [Double]
    let tint: Color

    var body: some View {
        Canvas { ctx, size in
            let center = CGPoint(x: size.width / 2, y: size.height / 2)
            let baseRadius = min(size.width, size.height) * 0.20
            let maxLen = min(size.width, size.height) * 0.28
            let count = values.count
            guard count > 0 else { return }

            // Glowing core orb.
            let orb = Path(ellipseIn: CGRect(
                x: center.x - baseRadius * 0.55, y: center.y - baseRadius * 0.55,
                width: baseRadius * 1.1, height: baseRadius * 1.1))
            ctx.fill(orb, with: .radialGradient(
                Gradient(colors: [tint.opacity(0.9), tint.opacity(0.0)]),
                center: center, startRadius: 0, endRadius: baseRadius * 0.8))

            // Faint guide ring.
            let ring = Path(ellipseIn: CGRect(
                x: center.x - baseRadius, y: center.y - baseRadius,
                width: baseRadius * 2, height: baseRadius * 2))
            ctx.stroke(ring, with: .color(tint.opacity(0.22)), lineWidth: 1.5)

            // 12 bands make a sparse wheel; interpolate to 4 spokes per band
            // (wrapping, so the circle has no seam) for a fuller corona.
            let spokes = Self.interpolated(values, factor: 4)
            for (index, value) in spokes.enumerated() {
                let angle = (Double(index) / Double(spokes.count)) * 2 * .pi - .pi / 2
                let inner = baseRadius + 4
                let outer = baseRadius + 4 + maxLen * value
                var spoke = Path()
                spoke.move(to: CGPoint(
                    x: center.x + cos(angle) * inner, y: center.y + sin(angle) * inner))
                spoke.addLine(to: CGPoint(
                    x: center.x + cos(angle) * outer, y: center.y + sin(angle) * outer))
                ctx.stroke(
                    spoke,
                    with: .linearGradient(
                        Gradient(colors: [tint.opacity(0.5), tint]),
                        startPoint: CGPoint(x: center.x + cos(angle) * inner, y: center.y + sin(angle) * inner),
                        endPoint: CGPoint(x: center.x + cos(angle) * outer, y: center.y + sin(angle) * outer)),
                    style: StrokeStyle(lineWidth: 3, lineCap: .round))
            }
        }
    }

    /// Linear interpolation around the ring, `factor` samples per band.
    static func interpolated(_ values: [Double], factor: Int) -> [Double] {
        guard values.count > 1 else { return values }
        return (0..<(values.count * factor)).map { i in
            let position = Double(i) / Double(factor)
            let lower = Int(position) % values.count
            let upper = (lower + 1) % values.count
            let t = position - Double(Int(position))
            return values[lower] * (1 - t) + values[upper] * t
        }
    }
}

/// A smooth, mirrored ribbon: a gradient-filled body between the top and bottom
/// curves with a bright accent stroke — flowing rather than a thin trace.
private struct WaveViz: View {
    let values: [Double]
    let tint: Color

    var body: some View {
        Canvas { ctx, size in
            let count = values.count
            guard count > 1 else { return }
            let mid = size.height / 2
            let step = size.width / CGFloat(count - 1)
            // A sine envelope pinches both ends to the centre line, so the
            // ribbon closes to a point instead of ending in a flat wall.
            func amp(_ i: Int) -> CGFloat {
                let envelope = sin(Double.pi * Double(i) / Double(count - 1))
                return CGFloat(values[i] * envelope) * size.height * 0.42
            }

            // Smooth top + bottom curves via quad curves through midpoints.
            func curve(sign: CGFloat) -> Path {
                var p = Path()
                p.move(to: CGPoint(x: 0, y: mid - sign * amp(0)))
                for i in 1..<count {
                    let x = CGFloat(i) * step
                    let y = mid - sign * amp(i)
                    let px = CGFloat(i - 1) * step
                    let py = mid - sign * amp(i - 1)
                    p.addQuadCurve(to: CGPoint(x: x, y: y),
                                   control: CGPoint(x: (px + x) / 2, y: py))
                }
                return p
            }
            let top = curve(sign: 1)
            let bottom = curve(sign: -1)

            // Filled ribbon between the two curves.
            var fill = top
            fill.addLine(to: CGPoint(x: size.width, y: mid + amp(count - 1)))
            for i in stride(from: count - 2, through: 0, by: -1) {
                fill.addLine(to: CGPoint(x: CGFloat(i) * step, y: mid + amp(i)))
            }
            fill.closeSubpath()
            ctx.fill(fill, with: .linearGradient(
                Gradient(colors: [tint.opacity(0.45), tint.opacity(0.08)]),
                startPoint: CGPoint(x: 0, y: 0), endPoint: CGPoint(x: 0, y: size.height)))

            // Bright edges.
            let stroke = StrokeStyle(lineWidth: 2.5, lineCap: .round, lineJoin: .round)
            ctx.stroke(top, with: .color(tint), style: stroke)
            ctx.stroke(bottom, with: .color(tint.opacity(0.6)), style: stroke)
        }
        .shadow(color: tint.opacity(0.4), radius: 10)
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
