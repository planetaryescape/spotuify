import SwiftUI
import SpotuifyKit

/// The navigation column, set like the track list on a sleeve: numbered
/// sections in the mono voice, pages in ink, the current one marked with an
/// accent tick. A live meter sits beside Now Playing. The update notice and
/// connection state sit at the foot, out of the content's way.
struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    @Binding var selection: Destination
    /// ↑/↓ navigation and the focus model live in `KeyboardController`;
    /// this column only draws the focus ring and applies the move.
    private var keyboard: KeyboardController { .shared }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            wordmark
                // Clear the window's traffic lights.
                .padding(.top, 46)
                .padding(.horizontal, 12)
                .padding(.bottom, 22)
            ScrollView(showsIndicators: false) {
                VStack(alignment: .leading, spacing: 22) {
                    ForEach(Array(Destination.Section.allCases.enumerated()), id: \.element) { index, section in
                        VStack(alignment: .leading, spacing: 1) {
                            MonoCaps(String(format: "%02d  %@", index + 1, section.rawValue), size: 9.5)
                                .padding(.horizontal, 12)
                                .padding(.bottom, 6)
                                .accessibilityAddTraits(.isHeader)
                            ForEach(Destination.sidebarOrder.filter { $0.section == section }) { destination in
                                SidebarRow(
                                    destination: destination,
                                    isSelected: destination == selection,
                                    showsFocus: keyboard.sidebarHasFocus && destination == selection
                                ) {
                                    selection = destination
                                }
                            }
                        }
                    }
                }
                .padding(.bottom, 16)
            }
            // Rows fade out under the footer rather than being cut by it.
            .mask(
                LinearGradient(
                    stops: [.init(color: .black, location: 0.9), .init(color: .clear, location: 1)],
                    startPoint: .top, endPoint: .bottom))
            .padding(.bottom, 8)
            VStack(alignment: .leading, spacing: 10) {
                UpdateNotice()
                connectionRow
            }
            .padding(.bottom, 14)
        }
        .padding(.horizontal, 10)
        .frame(maxHeight: .infinity, alignment: .top)
        .onAppear { keyboard.moveSidebarSelection = { move(by: $0) } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Navigation")
    }

    private func move(by step: Int) {
        let order = Destination.sidebarOrder
        guard let index = order.firstIndex(of: selection) else { return }
        let next = index + step
        guard order.indices.contains(next) else { return }
        selection = order[next]
    }

    /// "spotuify." — the full stop in the record's accent.
    private var wordmark: some View {
        (Text("spotuify").foregroundStyle(room.ink) + Text(".").foregroundStyle(room.accent))
            .font(.displayHero(25))
            .accessibilityLabel("spotuify")
            .accessibilityAddTraits(.isHeader)
    }

    private var connectionRow: some View {
        HStack(spacing: 7) {
            ConnectionDot(health: health)
            MonoCaps(badgeText, size: 9, color: health == .down ? ConnectionDot.down : nil)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .accessibilityElement(children: .combine)
    }

    private var health: ConnectionDot.Health {
        switch model.connectionState {
        case .ready: .ok
        case .connecting, .reconnecting, .idle: .working
        case .failed: .down
        }
    }

    private var badgeText: String {
        switch model.connectionState {
        case .idle: "Starting"
        case .connecting: "Connecting"
        case .reconnecting(let n): "Reconnecting · \(n)"
        case .ready: "Daemon connected"
        case .failed: "Daemon offline"
        }
    }
}

private struct SidebarRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    let destination: Destination
    let isSelected: Bool
    var showsFocus = false
    let action: () -> Void
    @State private var hovering = false

    private var shortcut: String? {
        guard let index = Navigator.numbered.firstIndex(of: destination) else { return nil }
        return index < 10 ? "⌘\((index + 1) % 10)" : "⇧⌘0"
    }

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: destination.icon)
                    .font(.system(size: 12.5, weight: .semibold))
                    .foregroundStyle(isSelected ? room.accent : room.inkFaint)
                    .frame(width: 18)
                Text(destination.title)
                    .font(.system(size: 13.5, weight: isSelected ? .semibold : .medium))
                    .foregroundStyle(isSelected ? room.ink : room.inkMuted)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if destination == .nowPlaying, model.player.currentItem != nil {
                    LevelMeter(isPlaying: model.player.isPlaying, size: 10)
                        .foregroundStyle(room.accent)
                } else if hovering, let shortcut {
                    Text(shortcut).font(.mono(9.5)).foregroundStyle(room.inkFaint)
                }
            }
            .padding(.horizontal, 12)
            .frame(height: 32)
            .background {
                RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
                    .fill(isSelected ? room.ink.opacity(0.08) : room.ink.opacity(hovering ? 0.04 : 0))
            }
            .overlay {
                if showsFocus {
                    RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
                        .strokeBorder(room.accent.opacity(0.55), lineWidth: 1.5)
                }
            }
            .overlay(alignment: .leading) {
                // The accent tick: the one saturated mark in the column.
                Capsule()
                    .fill(room.accent)
                    .frame(width: 3, height: isSelected ? 16 : 0)
                    .offset(x: -1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.8), value: isSelected)
        .accessibilityLabel(destination.title)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }
}
