import SwiftUI

/// Shared visual constants and small reusable styles. Dynamic, artwork-derived
/// accent colors land in Phase 9; this is the static foundation.
enum Theme {
    static let cornerRadius: CGFloat = 10
    static let artCornerRadius: CGFloat = 14
    static let sidebarWidth: CGFloat = 212
    static let nowPlayingBarHeight: CGFloat = 76

    static func timeString(_ ms: UInt64) -> String {
        let totalSeconds = Int(ms / 1000)
        return String(format: "%d:%02d", totalSeconds / 60, totalSeconds % 60)
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
