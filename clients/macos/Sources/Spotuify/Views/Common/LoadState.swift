import SwiftUI

enum LoadingStateStyle {
    case progress
    case rows
    case tiles
}

/// Shared first-load treatment. List and grid destinations reuse the app's
/// skeleton language; smaller surfaces can request a centered progress view.
struct LoadingStateView: View {
    let label: String
    var style: LoadingStateStyle = .progress

    var body: some View {
        Group {
            switch style {
            case .progress:
                VStack(spacing: Theme.Spacing.md) {
                    ProgressView()
                        .controlSize(.large)
                    MonoCaps(label, size: 10)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .rows:
                SkeletonRows()
            case .tiles:
                SkeletonTiles()
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
    }
}

/// Shared recoverable failure treatment for destination-level fetches.
struct ErrorStateView: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        EmptyState("Couldn't load this", systemImage: "exclamationmark.triangle", description: Text(message)) {
            Button("Try Again", action: retry)
                .buttonStyle(RoomButtonStyle(kind: .primary))
        }
    }
}

/// The room's empty state: a thin glyph, a Fraunces line, and a quiet
/// explanation — in place of the system's grey `ContentUnavailableView`.
/// Same call shape, so any empty surface can use it.
struct EmptyState<Actions: View>: View {
    @Environment(\.room) private var room
    let title: String
    let systemImage: String
    var description: Text?
    @ViewBuilder var actions: () -> Actions

    init(_ title: String, systemImage: String, description: Text? = nil,
         @ViewBuilder actions: @escaping () -> Actions) {
        self.title = title
        self.systemImage = systemImage
        self.description = description
        self.actions = actions
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: systemImage)
                .font(.system(size: 30, weight: .ultraLight))
                .foregroundStyle(room.accent)
                .frame(width: 72, height: 72)
                .background(Circle().strokeBorder(room.hairline, lineWidth: 1))
                .padding(.bottom, 4)
            Text(title)
                .font(.displayTitle(24))
                .foregroundStyle(room.ink)
                .multilineTextAlignment(.center)
            if let description {
                description
                    .font(.callout)
                    .foregroundStyle(room.inkMuted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 340)
            }
            actions().padding(.top, 6)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
    }
}

extension EmptyState where Actions == EmptyView {
    init(_ title: String, systemImage: String, description: Text? = nil) {
        self.init(title, systemImage: systemImage, description: description, actions: { EmptyView() })
    }
}

extension View {
    /// Occludes content scrolling under a pinned section header.
    func roomPinnedBackground() -> some View {
        modifier(RoomPinnedBackground())
    }
}

private struct RoomPinnedBackground: ViewModifier {
    @Environment(\.room) private var room
    func body(content: Content) -> some View {
        // Grain too, or the pinned strip reads as a flat black band.
        content.background { ZStack { room.base; Grain() } }
    }
}
