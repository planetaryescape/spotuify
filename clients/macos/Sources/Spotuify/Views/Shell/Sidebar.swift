import SwiftUI
import SpotuifyKit

/// The navigation sidebar — a native `List` so it adopts Tahoe's floating
/// Liquid Glass automatically. The Fraunces wordmark and connection badge are
/// pinned as top/bottom insets.
struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @Binding var selection: Destination

    var body: some View {
        List(selection: selectionBinding) {
            ForEach(Destination.allCases) { destination in
                Label(destination.title, systemImage: destination.icon)
                    .tag(destination)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .top, spacing: 0) {
            Text("spotuify")
                .font(.displayTitle(22))
                .foregroundStyle(.tint)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 18)
                .padding(.top, 10)
                .padding(.bottom, 6)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            connectionRow
        }
    }

    /// Sidebar single-selection wants an optional binding; never clear to nil.
    private var selectionBinding: Binding<Destination?> {
        Binding(get: { selection }, set: { if let value = $0 { selection = value } })
    }

    private var connectionRow: some View {
        HStack(spacing: 6) {
            Circle().fill(badgeColor).frame(width: 7, height: 7)
            Text(badgeText).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 10)
    }

    private var badgeColor: Color {
        switch model.connectionState {
        case .ready: .green
        case .connecting, .reconnecting: .yellow
        case .failed: .red
        case .idle: .gray
        }
    }

    private var badgeText: String {
        switch model.connectionState {
        case .idle: "Starting…"
        case .connecting: "Connecting…"
        case .reconnecting(let n): "Reconnecting (\(n))…"
        case .ready: "Connected"
        case .failed: "Daemon offline"
        }
    }
}
