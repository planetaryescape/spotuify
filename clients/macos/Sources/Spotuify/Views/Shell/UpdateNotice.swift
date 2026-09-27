import AppKit
import SwiftUI
import SpotuifyKit

/// "A newer release is available", as a small card at the foot of the sidebar.
/// It used to float over the top of the window, where it covered the stage's
/// mode switch and page headers. Shown only when auto-check is on and no error
/// banner is competing for attention.
struct UpdateNotice: View {
    @Environment(AppModel.self) private var model
    /// Mirrors the Settings toggle; the daemon's check itself is opt-out via env/config.
    @AppStorage("autoCheckUpdates") private var autoCheckUpdates = true

    var body: some View {
        if autoCheckUpdates, model.banner == nil, let update = model.availableUpdate {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "arrow.up.circle.fill").foregroundStyle(.tint)
                    Text(title(for: update))
                        .font(.caption.weight(.semibold))
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    Button { model.dismissUpdate() } label: {
                        Image(systemName: "xmark").font(.system(size: 9, weight: .bold))
                    }
                    .buttonStyle(.plain).foregroundStyle(.secondary)
                    .help("Dismiss")
                    // Dismissing mid-install orphaned a completed swap
                    // with no Relaunch button anywhere.
                    .disabled(model.updater.phase.isBusy)
                }
                actions(for: update)
            }
            .padding(10)
            .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(.tint.opacity(0.25)))
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }

    @ViewBuilder
    private func actions(for update: AvailableUpdate) -> some View {
        switch model.updater.phase {
        case .downloading, .verifying, .installing:
            ProgressView().controlSize(.small)
        case .installed(let url):
            Button("Relaunch") { AppRelaunch.relaunch(from: url) }
                .buttonStyle(RoomButtonStyle(kind: .primary)).controlSize(.small)
        case .failed:
            HStack(spacing: 6) {
                Button("Retry") {
                    model.updater.reset()
                    model.installAvailableUpdate()
                }
                .buttonStyle(RoomButtonStyle(kind: .primary)).controlSize(.small)
                if let urlString = update.url, let url = URL(string: urlString) {
                    Button("Releases") { NSWorkspace.shared.open(url) }
                        .buttonStyle(RoomButtonStyle()).controlSize(.small)
                }
            }
        case .idle:
            Button("Update Now") { model.installAvailableUpdate() }
                .buttonStyle(RoomButtonStyle(kind: .primary)).controlSize(.small)
        }
    }

    private func title(for update: AvailableUpdate) -> String {
        switch model.updater.phase {
        case .downloading: "Downloading \(update.latestVersion)…"
        case .verifying: "Verifying download…"
        case .installing: "Installing \(update.latestVersion)…"
        case .installed: "\(update.latestVersion) installed — relaunch to finish"
        case .failed(let message): message
        case .idle: "spotuify \(update.latestVersion) is available"
        }
    }
}
