import SwiftUI
import SpotuifyKit

struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @Binding var selection: Destination

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("spotuify")
                .font(.system(size: 18, weight: .bold, design: .rounded))
                .foregroundStyle(.tint)
                .padding(.horizontal, 14)
                .padding(.top, 14)
                .padding(.bottom, 10)

            ForEach(Destination.allCases) { destination in
                SidebarRow(destination: destination, isSelected: selection == destination) {
                    selection = destination
                }
            }

            Spacer()
            connectionRow
        }
        .frame(width: Theme.sidebarWidth)
        .background(.regularMaterial)
    }

    private var connectionRow: some View {
        HStack(spacing: 6) {
            Circle().fill(badgeColor).frame(width: 7, height: 7)
            Text(badgeText).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 12)
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

private struct SidebarRow: View {
    let destination: Destination
    let isSelected: Bool
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Label(destination.title, systemImage: destination.icon)
                .font(.system(size: 13, weight: isSelected ? .semibold : .regular))
                .foregroundStyle(isSelected ? AnyShapeStyle(.tint) : AnyShapeStyle(.primary))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 7)
                .padding(.horizontal, 10)
                .background {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(background)
                }
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 8)
        .onHover { hovering = $0 }
    }

    private var background: AnyShapeStyle {
        if isSelected { AnyShapeStyle(.tint.opacity(0.15)) }
        else if hovering { AnyShapeStyle(.primary.opacity(0.06)) }
        else { AnyShapeStyle(.clear) }
    }
}
