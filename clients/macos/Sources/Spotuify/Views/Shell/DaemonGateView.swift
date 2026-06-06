import SwiftUI
import SpotuifyKit

/// Shown until the daemon is confirmed present AND new enough. Replaces the
/// player UI with actionable install / start / upgrade instructions.
struct DaemonGateView: View {
    @Environment(AppModel.self) private var model
    let readiness: DaemonReadiness

    @State private var working = false
    @State private var workingLabel = ""
    @State private var actionError: String?

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: icon)
                .font(.system(size: 52))
                .foregroundStyle(.tint)
                .symbolEffect(.pulse, isActive: isChecking)

            VStack(spacing: 8) {
                Text(title).font(.displayHero(30)).multilineTextAlignment(.center)
                Text(subtitle)
                    .font(.callout).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 460)
            }

            if isChecking {
                ProgressView().controlSize(.large)
            } else if working {
                VStack(spacing: 10) {
                    ProgressView()
                    Text(workingLabel).font(.callout).foregroundStyle(.secondary)
                }
            } else {
                actionArea
            }
        }
        .padding(40)
        .frame(minWidth: 620, minHeight: 520)
        .background(.background)
    }

    private var isChecking: Bool { readiness == .checking }

    private var icon: String {
        switch readiness {
        case .checking: "antenna.radiowaves.left.and.right"
        case .missing(let installed): installed ? "bolt.horizontal.circle" : "shippingbox"
        case .incompatible: "arrow.up.circle"
        case .ready: "checkmark.circle"
        }
    }

    private var title: String {
        switch readiness {
        case .checking: "Connecting to spotuify…"
        case .missing(let installed): installed ? "The spotuify daemon isn’t running" : "spotuify isn’t installed"
        case .incompatible: "Your spotuify daemon is out of date"
        case .ready: "Connected"
        }
    }

    private var subtitle: String {
        switch readiness {
        case .checking:
            return "Looking for the spotuify daemon."
        case .missing(let installed):
            return installed
                ? "Spotuify found the binary but the daemon didn’t start. Start it, then retry. If it keeps failing, run spotuify doctor."
                : "Spotuify is the backend that plays your music. Install it, then this app connects automatically."
        case .incompatible(let found, let required, let version):
            return "This app needs daemon protocol v\(required), but the running daemon (\(version)) speaks v\(found). Upgrade the daemon and restart it."
        case .ready:
            return ""
        }
    }

    private var commands: [String] {
        switch readiness {
        case .missing(let installed):
            if installed {
                return ["spotuify daemon start", "spotuify doctor"]
            }
            return [
                "brew install planetaryescape/spotuify/spotuify",
                "spotuify daemon start",
            ]
        case .incompatible:
            return [
                "brew upgrade planetaryescape/spotuify/spotuify",
                "spotuify daemon restart",
            ]
        default:
            return []
        }
    }

    // MARK: One-click actions

    @ViewBuilder
    private var actionArea: some View {
        VStack(spacing: 16) {
            if let action = primaryAction {
                Button { perform(action) } label: {
                    Label(action.title, systemImage: action.icon)
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
            }

            if let actionError {
                Text(actionError)
                    .font(.caption).foregroundStyle(.red)
                    .multilineTextAlignment(.center).frame(maxWidth: 460)
            }

            if !commands.isEmpty {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(Array(commands.enumerated()), id: \.offset) { _, command in
                        CommandRow(command: command)
                    }
                }
                .frame(maxWidth: 520)
            }

            HStack(spacing: 12) {
                if primaryAction == nil {
                    Button { model.forceReconnect() } label: {
                        Label("Retry", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(.borderedProminent)
                } else {
                    Button { model.forceReconnect() } label: {
                        Label("Retry", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(.bordered)
                }
                if !commands.isEmpty {
                    Button { TerminalLauncher.run(commands) } label: {
                        Label("Open in Terminal", systemImage: "terminal")
                    }
                    .buttonStyle(.bordered)
                }
                Link("Docs", destination: URL(string: "https://spotuify.vercel.app")!)
                    .buttonStyle(.bordered)
            }
        }
    }

    private enum GateAction {
        case start, updateRestart
        var title: String {
            switch self {
            case .start: "Start spotuify"
            case .updateRestart: "Update & Restart"
            }
        }
        var icon: String {
            switch self {
            case .start: "play.fill"
            case .updateRestart: "arrow.down.circle"
            }
        }
    }

    /// A one-click action for the cases we can drive automatically.
    private var primaryAction: GateAction? {
        switch readiness {
        case .missing(let installed): installed ? .start : nil
        case .incompatible: .updateRestart
        default: nil
        }
    }

    private func perform(_ action: GateAction) {
        actionError = nil
        working = true
        workingLabel = action == .start
            ? "Starting spotuify…"
            : "Updating spotuify… (this can take a minute)"
        Task {
            defer { working = false }
            switch action {
            case .start:
                if await DaemonControl.startDaemon(socketPath: model.socketPath) {
                    model.forceReconnect()
                } else {
                    actionError = "Couldn’t start the daemon automatically. Try the commands below, or open Terminal."
                }
            case .updateRestart:
                let result = await DaemonControl.updateViaBrew(socketPath: model.socketPath)
                if result.ok {
                    model.forceReconnect()
                } else {
                    actionError = "Update didn’t finish — opening Terminal so you can run it and watch the output."
                    TerminalLauncher.run(DaemonControl.brewUpdateCommands)
                }
            }
        }
    }
}

/// A copyable monospaced command line.
private struct CommandRow: View {
    let command: String
    @State private var copied = false

    var body: some View {
        HStack {
            Text(command)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
            Spacer(minLength: 12)
            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(command, forType: .string)
                copied = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { copied = false }
            } label: {
                Image(systemName: copied ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("Copy")
        }
        .padding(.horizontal, 12).padding(.vertical, 9)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
    }
}
