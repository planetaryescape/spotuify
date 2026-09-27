import SwiftUI

/// Shared visual constants and small reusable styles. Cover-derived color lives
/// in `ArtworkPalette`/`ArtworkTheme`; the editorial type tier lives in
/// `EditorialFont` (Fraunces). This holds the static layout tokens.
enum Theme {
    enum Spacing {
        static let xs: CGFloat = 4
        static let sm: CGFloat = 8
        static let md: CGFloat = 12
        static let lg: CGFloat = 16
        static let xl: CGFloat = 20
        static let xxl: CGFloat = 24
    }

    static let cornerRadius: CGFloat = 10
    static let rowRadius: CGFloat = 8
    static let chipRadius: CGFloat = 8
    static let artCornerRadius: CGFloat = 14
    static let tileCornerRadius: CGFloat = 12
    static let sidebarWidth: CGFloat = 224
    static let nowPlayingBarHeight: CGFloat = 92

    enum TrackColumn {
        static let artwork: CGFloat = 40
        static let album: CGFloat = 220
        static let dateAdded: CGFloat = 90
        static let actions: CGFloat = 56
        static let duration: CGFloat = 48
    }

    static func timeString(_ ms: UInt64) -> String {
        let totalSeconds = Int(ms / 1000)
        return String(format: "%d:%02d", totalSeconds / 60, totalSeconds % 60)
    }
}

/// Standard page masthead: a mono eyebrow over a big Fraunces title, with an
/// optional trailing accessory. Used at the top of every destination.
struct EditorialPageHeader<Trailing: View>: View {
    let title: String
    var eyebrow: String?
    @ViewBuilder var trailing: () -> Trailing
    @Environment(\.room) private var room

    var body: some View {
        HStack(alignment: .lastTextBaseline) {
            VStack(alignment: .leading, spacing: 6) {
                if let eyebrow { MonoCaps(eyebrow, size: 10) }
                Text(title)
                    .font(.displayHero(40))
                    .foregroundStyle(room.ink)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                    .accessibilityAddTraits(.isHeader)
            }
            Spacer()
            trailing()
        }
        .padding(.horizontal, 32)
        .padding(.top, 44)
        .padding(.bottom, 18)
    }
}

/// Convenience initialiser for a trailing-less `EditorialPageHeader`.
extension EditorialPageHeader where Trailing == EmptyView {
    init(_ title: String, eyebrow: String? = nil) {
        self.init(title: title, eyebrow: eyebrow, trailing: { EmptyView() })
    }
}

extension EditorialPageHeader {
    init(title: String, @ViewBuilder trailing: @escaping () -> Trailing) {
        self.init(title: title, eyebrow: nil, trailing: trailing)
    }
}

extension View {
    /// Clips artwork to a true circle instead of approximating one with an
    /// oversized corner radius. Passing `false` preserves the existing shape.
    func circularArtwork(_ enabled: Bool = true) -> some View {
        clipShape(enabled ? AnyShape(Circle()) : AnyShape(Rectangle()))
    }

    /// The room's input field: a faint ink capsule with a hairline.
    func glassField() -> some View {
        modifier(RoomField())
    }

    /// A small Fraunces section heading for grouped lists.
    func editorialSectionHeader() -> some View {
        font(.displayTitle(20))
    }
}

extension View {
    /// Subtle hover-highlightable row used in lists.
    func selectableRowBackground(_ selected: Bool) -> some View {
        background {
            RoundedRectangle(cornerRadius: Theme.cornerRadius)
                .fill(selected ? AnyShapeStyle(.tint.opacity(0.18)) : AnyShapeStyle(.clear))
        }
    }
}

/// A transport icon button with a consistent hit area and hover feel.
struct TransportButton: View {
    let systemName: String
    var size: CGFloat = 16
    var prominent: Bool = false
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: size, weight: .semibold))
                .frame(width: prominent ? 44 : 32, height: prominent ? 44 : 32)
                .background {
                    if prominent {
                        Circle().fill(.tint)
                    } else {
                        Circle().fill(hovering ? AnyShapeStyle(.primary.opacity(0.08)) : AnyShapeStyle(.clear))
                    }
                }
                .foregroundStyle(prominent ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}

private struct RoomField: ViewModifier {
    @Environment(\.room) private var room
    func body(content: Content) -> some View {
        content
            .textFieldStyle(.plain)
            .padding(.horizontal, 14)
            .frame(height: 34)
            .background(Capsule().fill(room.ink.opacity(0.06)))
            .overlay(Capsule().strokeBorder(room.ink.opacity(0.12)))
    }
}
