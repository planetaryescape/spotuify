import AppKit
import SwiftUI

/// The room's colour tokens. Every surface reads these instead of system
/// materials, so the whole window takes the record's colour and the app keeps
/// one face across pages. Derived from `ArtworkTheme.palette`.
struct Room: Equatable {
    /// The floor every page sits on.
    var base: Color
    /// One step up: the deck, cards, hovered rows.
    var raised: Color
    var ink: Color
    var inkMuted: Color
    var inkFaint: Color
    var hairline: Color
    var accent: Color
    var isLight: Bool

    /// Warm paper-white ink: pure white glares against a tinted dark room.
    private static let paperInk = Color(red: 0.97, green: 0.95, blue: 0.92)
    private static let paper = Color(red: 0.96, green: 0.95, blue: 0.925)

    static func derive(from palette: ArtworkPalette, accent: Color, light: Bool) -> Room {
        if light {
            let ink = Color(red: 0.09, green: 0.08, blue: 0.07)
            return Room(
                base: paper,
                raised: Color(red: 0.99, green: 0.985, blue: 0.97),
                ink: ink,
                inkMuted: ink.opacity(0.62),
                inkFaint: ink.opacity(0.4),
                hairline: ink.opacity(0.1),
                accent: accent,
                isLight: true)
        }
        return Room(
            base: palette.background.mix(with: .black, by: 0.62),
            raised: palette.background.mix(with: .black, by: 0.42),
            ink: paperInk,
            inkMuted: paperInk.opacity(0.64),
            inkFaint: paperInk.opacity(0.4),
            hairline: paperInk.opacity(0.09),
            accent: accent,
            isLight: false)
    }
}

extension EnvironmentValues {
    @Entry var room: Room = .derive(from: .darkFallback, accent: .accentColor, light: false)
}

extension ArtworkTheme {
    var room: Room { Room.derive(from: palette, accent: accent, light: immersiveIsLight) }
}

// MARK: - Floor

/// The floor behind every page: the room's base, a faint glow of the current
/// cover in the top corner, and grain.
struct RoomFloor: View {
    @Environment(\.room) private var room
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    let imageURL: String?

    var body: some View {
        ZStack(alignment: .topTrailing) {
            room.base
            if !reduceTransparency {
                AsyncCoverImage(url: imageURL, cornerRadius: 0, showsPlaceholderGlyph: false)
                    .frame(width: 520, height: 520)
                    .blur(radius: 120)
                    .opacity(room.isLight ? 0.22 : 0.4)
                    .offset(x: 160, y: -220)
                    .id(imageURL)
                    .transition(.opacity)
            }
            Grain()
        }
        .animation(.easeInOut(duration: 0.9), value: imageURL)
        .clipped()
        .ignoresSafeArea()
        // Decoration only; see NowPlayingBackdrop for why this matters.
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Film grain. A fixed noise tile, generated once, laid over the room so flat
/// colour reads as a surface instead of a screen.
struct Grain: View {
    @Environment(\.room) private var room
    var intensity: Double = 1

    var body: some View {
        Image(decorative: Self.tile, scale: 2)
            .resizable(resizingMode: .tile)
            .opacity((room.isLight ? 0.05 : 0.07) * intensity)
            .blendMode(room.isLight ? .multiply : .screen)
            .allowsHitTesting(false)
    }

    /// 192px of luminance noise; seeded so the texture is stable between launches.
    private static let tile: CGImage = {
        let size = 192
        var generator = SplitMix64(seed: 0x5EED_5907)
        var pixels = [UInt8](repeating: 0, count: size * size)
        for i in pixels.indices {
            pixels[i] = UInt8(truncatingIfNeeded: generator.next() >> 56)
        }
        let provider = CGDataProvider(data: Data(pixels) as CFData)!
        return CGImage(
            width: size, height: size, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: size,
            space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: 0),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
    }()
}

/// Tiny deterministic PRNG for the grain tile (Swift's default generator
/// can't be seeded).
private struct SplitMix64 {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}

// MARK: - Voice

extension Font {
    /// The facts voice: monospaced, for times, counts, eyebrows, section numbers.
    static func mono(_ size: CGFloat, weight: Font.Weight = .medium) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
}

/// Small monospaced caps with open tracking — eyebrows and section labels.
struct MonoCaps: View {
    let text: String
    var size: CGFloat = 10
    var color: Color?
    @Environment(\.room) private var room

    init(_ text: String, size: CGFloat = 10, color: Color? = nil) {
        self.text = text
        self.size = size
        self.color = color
    }

    var body: some View {
        Text(text.uppercased())
            .font(.mono(size, weight: .semibold))
            .tracking(size * 0.14)
            .foregroundStyle(color ?? room.inkFaint)
            .lineLimit(1)
    }
}

// MARK: - Controls

/// A capsule button. `.primary` is filled with the accent; `.quiet` is an
/// ink outline that fills faintly on hover.
struct RoomButtonStyle: ButtonStyle {
    enum Kind { case primary, quiet }
    var kind: Kind = .quiet
    @Environment(\.room) private var room
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        RoomButtonBody(configuration: configuration, kind: kind, room: room, isEnabled: isEnabled)
    }
}

private struct RoomButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let kind: RoomButtonStyle.Kind
    let room: Room
    let isEnabled: Bool
    @State private var hovering = false

    var body: some View {
        configuration.label
            .font(.system(size: 12.5, weight: .semibold))
            .labelStyle(RoomLabelStyle())
            .padding(.horizontal, 14)
            .frame(height: 30)
            .foregroundStyle(kind == .primary ? (room.isLight ? Color.white : Color.black) : room.ink)
            .background {
                switch kind {
                case .primary:
                    Capsule().fill(room.accent.opacity(hovering ? 0.88 : 1))
                case .quiet:
                    Capsule().fill(room.ink.opacity(hovering ? 0.1 : 0.04))
                    Capsule().strokeBorder(room.ink.opacity(0.16))
                }
            }
            .contentShape(Capsule())
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .opacity(isEnabled ? 1 : 0.4)
            .onHover { hovering = $0 }
            .animation(.spring(response: 0.22, dampingFraction: 0.7), value: configuration.isPressed)
            .animation(.easeOut(duration: 0.12), value: hovering)
    }
}

private struct RoomLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.font(.system(size: 11, weight: .bold))
            configuration.title
        }
    }
}

/// A round icon button in ink: the hero's shuffle / queue / save actions.
struct RoomIconButton: View {
    let systemName: String
    let label: String
    var diameter: CGFloat = 38
    var isOn = false
    let action: () -> Void
    @Environment(\.room) private var room
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: diameter * 0.36, weight: .semibold))
                .foregroundStyle(isOn ? room.accent : room.ink.opacity(hovering ? 1 : 0.8))
                .frame(width: diameter, height: diameter)
                .background(Circle().fill(room.ink.opacity(hovering ? 0.1 : 0.04)))
                .overlay(Circle().strokeBorder(room.ink.opacity(0.14)))
                .contentShape(Circle())
                .contentTransition(.symbolEffect(.replace))
        }
        .buttonStyle(PressableButtonStyle())
        .onHover { hovering = $0 }
        .help(label)
        .accessibilityLabel(label)
    }
}

/// The big round accent play button that anchors every hero.
struct RoomPlayButton: View {
    let label: String
    var diameter: CGFloat = 52
    let action: () -> Void
    @Environment(\.room) private var room

    var body: some View {
        Button(action: action) {
            Image(systemName: "play.fill")
                .font(.system(size: diameter * 0.38, weight: .bold))
                .offset(x: diameter * 0.03)
                .foregroundStyle(room.isLight ? Color.white : Color.black)
                .frame(width: diameter, height: diameter)
                .background(Circle().fill(room.accent))
                .shadow(color: room.accent.opacity(0.45), radius: 14, y: 6)
                .contentShape(Circle())
        }
        .buttonStyle(PressableButtonStyle())
        .help(label)
        .accessibilityLabel(label)
    }
}

/// "‹ Albums" — the back link for pushed pages (the window has no toolbar to
/// hold a system back button).
struct RoomBackButton: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.room) private var room
    @State private var hovering = false

    var body: some View {
        Button { dismiss() } label: {
            HStack(spacing: 6) {
                Image(systemName: "chevron.left").font(.system(size: 10, weight: .bold))
                MonoCaps("Back", size: 10, color: hovering ? room.ink : room.inkMuted)
            }
            .padding(.horizontal, 10).frame(height: 26)
            .background(Capsule().fill(room.ink.opacity(hovering ? 0.1 : 0.05)))
            .foregroundStyle(hovering ? room.ink : room.inkMuted)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .keyboardShortcut("[", modifiers: .command)
        .help("Back (⌘[)")
    }
}

/// Text tabs with a sliding underline — used instead of a segmented control.
struct RoomTabs<Value: Hashable>: View {
    let options: [(value: Value, title: String)]
    @Binding var selection: Value
    var color: Color?
    @Environment(\.room) private var room
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var underline

    var body: some View {
        HStack(spacing: 22) {
            ForEach(options, id: \.value) { option in
                let active = option.value == selection
                Button {
                    selection = option.value
                } label: {
                    VStack(spacing: 6) {
                        MonoCaps(option.title, size: 10.5,
                                 color: active ? (color ?? room.ink) : (color ?? room.ink).opacity(0.5))
                        ZStack {
                            Capsule().fill(.clear).frame(height: 2)
                            if active {
                                Capsule().fill(room.accent).frame(height: 2)
                                    .matchedGeometryEffect(id: "underline", in: underline)
                            }
                        }
                    }
                    .fixedSize()
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(active ? [.isSelected] : [])
            }
        }
        .animation(reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.8), value: selection)
    }
}

/// A menu picker whose label is set in the mono voice — "SORT · TITLE ⌄" —
/// instead of the system pop-up button. The menu itself stays native.
struct RoomMenuPicker<Value: Hashable>: View {
    let label: String
    let options: [(value: Value, title: String)]
    @Binding var selection: Value
    @Environment(\.room) private var room

    var body: some View {
        Menu {
            Picker(label, selection: $selection) {
                ForEach(options, id: \.value) { Text($0.title).tag($0.value) }
            }
            .pickerStyle(.inline)
        } label: {
            HStack(spacing: 5) {
                MonoCaps("\(label) · \(options.first { $0.value == selection }?.title ?? "")", size: 9.5, color: room.inkMuted)
                Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold)).foregroundStyle(room.inkFaint)
            }
            .padding(.horizontal, 10).frame(height: 26)
            .background(Capsule().fill(room.ink.opacity(0.05)))
            .contentShape(Capsule())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
    }
}

/// A section label inside a page: mono caps with a hairline running to the edge.
struct RoomSectionLabel: View {
    let text: String
    @Environment(\.room) private var room

    init(_ text: String) { self.text = text }

    var body: some View {
        HStack(spacing: 10) {
            MonoCaps(text, size: 10)
            Rectangle().fill(room.hairline).frame(height: 1)
        }
        .padding(.top, 18).padding(.bottom, 6)
        .accessibilityAddTraits(.isHeader)
    }
}

/// Daemon connection as a small dot. Quiet when healthy, loud only when
/// something needs you: a green "all fine" light was the brightest thing in
/// the sidebar and fought the record's palette.
struct ConnectionDot: View {
    enum Health { case ok, working, down }
    let health: Health
    var size: CGFloat = 6
    @Environment(\.room) private var room
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pulse = false

    /// Desaturated so even the alarm sits in the room's warm, dim register.
    static let down = Color(red: 0.86, green: 0.38, blue: 0.32)

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .opacity(health == .working && pulse ? 0.35 : 1)
            .animation(
                health == .working && !reduceMotion
                    ? .easeInOut(duration: 0.8).repeatForever(autoreverses: true) : .default,
                value: pulse)
            .onAppear { pulse = health == .working }
            .onChange(of: health) { _, new in pulse = new == .working }
            .accessibilityHidden(true)
    }

    private var color: Color {
        switch health {
        case .ok: room.inkFaint
        case .working: room.accent
        case .down: Self.down
        }
    }
}
