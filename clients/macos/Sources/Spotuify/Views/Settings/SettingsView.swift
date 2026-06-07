import SwiftUI
import SpotuifyKit

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @AppStorage("autoCheckUpdates") private var autoCheckUpdates = true

    var body: some View {
        Form {
            Section("Updates") {
                Toggle("Check for updates automatically", isOn: $autoCheckUpdates)
                if let update = model.availableUpdate {
                    LabeledContent("Available") {
                        Text("spotuify \(update.latestVersion)").foregroundStyle(.tint)
                    }
                }
                Button("Check Now") { model.checkUpdate(force: true) }
            }

            Section("Daemon") {
                LabeledContent("Status") {
                    HStack(spacing: 6) {
                        Circle().fill(statusColor).frame(width: 8, height: 8)
                        Text(statusText)
                    }
                }
                LabeledContent("Socket") {
                    Text(model.socketPath)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .foregroundStyle(.secondary)
                }
                Button("Reconnect") { model.forceReconnect() }
            }

            Section("About") {
                LabeledContent("Spotuify", value: "macOS client")
                Text("A native player for the spotuify daemon — the same daemon the CLI and TUI drive. Playback runs in the daemon; this app is a view.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 460, height: 420)
    }

    private var statusColor: Color {
        switch model.connectionState {
        case .ready: .green
        case .connecting, .reconnecting: .yellow
        case .failed: .red
        case .idle: .gray
        }
    }

    private var statusText: String {
        switch model.connectionState {
        case .idle: "Idle"
        case .connecting: "Connecting"
        case .reconnecting(let n): "Reconnecting (\(n))"
        case .ready: "Connected"
        case .failed: "Offline"
        }
    }
}
