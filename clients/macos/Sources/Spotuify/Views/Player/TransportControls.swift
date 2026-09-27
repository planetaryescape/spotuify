import SwiftUI
import SpotuifyKit

/// Shrinks a control slightly while pressed so clicks feel physical. Motion is
/// skipped under Reduce Motion; the press still dims.
struct PressableButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.9 : 1)
            .opacity(isEnabled ? (configuration.isPressed ? 0.75 : 1) : 0.35)
            .animation(.spring(response: 0.22, dampingFraction: 0.6), value: configuration.isPressed)
    }
}

/// The round play/pause button. The glyph morphs between play and pause with
/// the system symbol transition rather than swapping abruptly.
struct PlayPauseButton: View {
    @Environment(AppModel.self) private var model
    var diameter: CGFloat = 44
    var fill: AnyShapeStyle = AnyShapeStyle(.tint)
    var glyph: Color = .white

    var body: some View {
        Button { model.togglePlayPause() } label: {
            Image(systemName: model.player.isPlaying ? "pause.fill" : "play.fill")
                .font(.system(size: diameter * 0.4, weight: .bold))
                .contentTransition(.symbolEffect(.replace.downUp))
                // Optical centring: the play triangle's mass sits left of its box.
                .offset(x: model.player.isPlaying ? 0 : diameter * 0.03)
                .foregroundStyle(glyph)
                .frame(width: diameter, height: diameter)
                .background(fill, in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(PressableButtonStyle())
        .disabled(!model.canTogglePlayPause)
        .help(model.player.isPlaying ? "Pause (Space)" : "Play (Space)")
        .accessibilityLabel(model.player.isPlaying ? "Pause" : "Play")
    }
}

/// A borderless transport glyph with a generous hit target and a hover wash.
/// `isOn` adds a dot under the glyph, so shuffle and repeat state never relies
/// on colour alone.
struct TransportGlyph: View {
    let systemName: String
    let label: String
    var size: CGFloat = 15
    var isOn: Bool?
    var color: Color = .primary
    var onColor: Color = .accentColor
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: size, weight: .semibold))
                .foregroundStyle(isOn == true ? onColor : color.opacity(isOn == false ? 0.62 : 1))
                .frame(width: size * 2.2, height: size * 2.2)
                .background(color.opacity(hovering ? 0.1 : 0), in: Circle())
                .overlay(alignment: .bottom) {
                    if isOn == true {
                        Circle().fill(onColor).frame(width: 4, height: 4).offset(y: -size * 0.1)
                    }
                }
                .contentShape(Circle())
        }
        .buttonStyle(PressableButtonStyle())
        .onHover { hovering = $0 }
        .help(label)
        .accessibilityLabel(label)
        .accessibilityValue(isOn.map { $0 ? "On" : "Off" } ?? "")
    }
}

/// Shuffle · previous · play/pause · next · repeat, sized for the surface it
/// sits on. One definition drives the stage, the dock, the menu bar and the
/// mini player, so the controls behave the same everywhere.
struct TransportCluster: View {
    enum Scale {
        case compact, regular, large

        var glyph: CGFloat { switch self { case .compact: 13; case .regular: 17; case .large: 22 } }
        var mode: CGFloat { switch self { case .compact: 11; case .regular: 13; case .large: 15 } }
        var play: CGFloat { switch self { case .compact: 34; case .regular: 46; case .large: 64 } }
        var spacing: CGFloat { switch self { case .compact: 6; case .regular: 12; case .large: 22 } }
    }

    @Environment(AppModel.self) private var model
    var scale: Scale = .regular
    var showsModes = true
    /// Glyph colour for skip/mode buttons.
    var color: Color = .primary
    /// Colour for active shuffle/repeat.
    var onColor: Color = .accentColor
    var playFill: AnyShapeStyle = AnyShapeStyle(.tint)
    var playGlyph: Color = .white

    var body: some View {
        HStack(spacing: scale.spacing) {
            if showsModes {
                TransportGlyph(
                    systemName: "shuffle", label: "Shuffle", size: scale.mode,
                    isOn: model.player.shuffle, color: color, onColor: onColor
                ) { model.toggleShuffle() }
                    .disabled(!model.canSetShuffle)
            }
            TransportGlyph(systemName: "backward.fill", label: "Previous", size: scale.glyph, color: color) {
                model.previous()
            }
            .disabled(!model.canSkipPrevious)
            PlayPauseButton(diameter: scale.play, fill: playFill, glyph: playGlyph)
            TransportGlyph(systemName: "forward.fill", label: "Next", size: scale.glyph, color: color) {
                model.next()
            }
            .disabled(!model.canSkipNext)
            if showsModes {
                TransportGlyph(
                    systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat",
                    label: repeatLabel, size: scale.mode,
                    isOn: model.player.repeatMode != .off, color: color, onColor: onColor
                ) { model.cycleRepeat() }
                    .disabled(!model.canSetRepeat)
            }
        }
    }

    private var repeatLabel: String {
        switch model.player.repeatMode {
        case .off: "Repeat"
        case .context: "Repeat all"
        case .track: "Repeat one"
        }
    }
}

/// A seek bar flanked by elapsed and remaining time, in tabular digits so the
/// numbers don't jitter as they tick.
struct SeekRow: View {
    @Environment(AppModel.self) private var model
    var barHeight: CGFloat = 4
    var fill: AnyShapeStyle = AnyShapeStyle(.tint)
    var textColor: Color = .secondary
    /// `.inline` puts the times beside the bar; `.stacked` puts them under it.
    var layout: Layout = .inline

    enum Layout { case inline, stacked }

    var body: some View {
        switch layout {
        case .inline:
            HStack(spacing: 10) {
                elapsed.frame(width: 38, alignment: .trailing)
                bar
                remaining.frame(width: 38, alignment: .leading)
            }
        case .stacked:
            VStack(spacing: 4) {
                bar
                HStack { elapsed; Spacer(); remaining }
            }
        }
    }

    private var bar: some View {
        SeekBar(
            progress: model.player.progressFraction,
            durationMs: model.player.durationMs,
            onSeek: { model.seek(toFraction: $0) },
            height: barHeight,
            fill: fill)
            .disabled(!model.canSeek)
    }

    private var elapsed: some View {
        Text(Theme.timeString(model.player.displayProgressMs))
            .font(.caption2.monospacedDigit().weight(.medium))
            .foregroundStyle(textColor)
            .accessibilityHidden(true)
    }

    private var remaining: some View {
        let duration = model.player.durationMs
        let left = duration > model.player.displayProgressMs ? duration - model.player.displayProgressMs : 0
        return Text(duration > 0 ? "-\(Theme.timeString(left))" : "0:00")
            .font(.caption2.monospacedDigit().weight(.medium))
            .foregroundStyle(textColor)
            .accessibilityHidden(true)
    }
}

/// A small live level meter: animates while music plays, rests flat when it
/// doesn't. Used beside "Now Playing" in the sidebar and on the playing row.
struct LevelMeter: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let isPlaying: Bool
    var size: CGFloat = 11

    var body: some View {
        Image(systemName: "waveform")
            .font(.system(size: size, weight: .bold))
            .symbolEffect(.variableColor.iterative.dimInactiveLayers.reversing, isActive: isPlaying && !reduceMotion)
            .accessibilityLabel(isPlaying ? "Playing" : "Paused")
    }
}

/// Podcast speed, shown as the current rate ("1.5×") whenever an episode is
/// playing. For podcasts speed is a primary control, so it sits next to the
/// transport rather than in an overflow menu, where it was hard to find.
struct PlaybackSpeedButton: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    /// Ink for the label; the stage passes its immersive text colour.
    var color: Color?

    var body: some View {
        if model.player.currentItemIsEpisode {
            Menu {
                Picker("Playback Speed", selection: Binding(
                    get: { model.podcastSpeed },
                    set: { model.setPodcastSpeed($0) })
                ) {
                    ForEach(PlaybackSpeedInfo.presets, id: \.self) { speed in
                        Text(PlaybackSpeedInfo.label(speed)).tag(speed)
                    }
                }
                .pickerStyle(.inline)
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "gauge.with.dots.needle.67percent")
                        .font(.system(size: 11, weight: .semibold))
                    Text(PlaybackSpeedInfo.label(model.podcastSpeed))
                        .font(.mono(11, weight: .semibold))
                }
                .foregroundStyle(isAdjusted ? room.accent : (color ?? room.ink).opacity(0.85))
                .padding(.horizontal, 9)
                .frame(height: 26)
                .background(Capsule().fill((color ?? room.ink).opacity(isAdjusted ? 0.12 : 0.07)))
                .contentShape(Capsule())
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Podcast playback speed")
            .accessibilityLabel("Playback speed")
            .accessibilityValue(PlaybackSpeedInfo.label(model.podcastSpeed))
            .task { await model.loadPodcastSpeed() }
        }
    }

    private var isAdjusted: Bool { abs(model.podcastSpeed - 1.0) > 0.001 }
}
