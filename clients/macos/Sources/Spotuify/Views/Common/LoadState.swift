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
                    Text(label)
                        .font(.callout)
                        .foregroundStyle(.secondary)
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
        ContentUnavailableView {
            Label("Couldn't load content", systemImage: "exclamationmark.triangle")
        } description: {
            Text(message)
        } actions: {
            Button("Retry", action: retry)
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
