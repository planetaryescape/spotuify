import AppKit
import SwiftUI
import SpotuifyKit

/// Full Settings window: a sidebar of panes that visually edit the daemon config
/// (via `model.config`, which drives the `spotuify config` CLI) plus Updates,
/// Daemon, and About. Edits write through `config set` + `reload`.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    @State private var pane: Pane = .account
    /// The desktop appearance choice, applied app-wide from the app root
    /// (`DesktopThemeModifier`). Defaults to `.adaptive` to preserve behavior.
    @AppStorage("desktopTheme") private var desktopThemeRaw = AppTheme.adaptive.rawValue

    private var desktopTheme: Binding<AppTheme> {
        Binding(
            get: { AppTheme(rawValue: desktopThemeRaw) ?? .adaptive },
            set: { desktopThemeRaw = $0.rawValue })
    }

    enum Pane: String, CaseIterable, Identifiable {
        case account, appearance, playback, audio, notifications, privacy, updates, daemon, about
        var id: String { rawValue }
        var title: String {
            switch self {
            case .account: "Account"
            case .appearance: "Appearance"
            case .playback: "Playback"
            case .audio: "Audio Output"
            case .notifications: "Notifications"
            case .privacy: "Privacy & Cache"
            case .updates: "Updates"
            case .daemon: "Daemon"
            case .about: "About"
            }
        }
        var icon: String {
            switch self {
            case .account: "person.crop.circle"
            case .appearance: "paintbrush"
            case .playback: "play.circle"
            case .audio: "hifispeaker"
            case .notifications: "bell"
            case .privacy: "lock.shield"
            case .updates: "arrow.down.circle"
            case .daemon: "bolt.horizontal.circle"
            case .about: "info.circle"
            }
        }
    }

    var body: some View {
        HStack(spacing: 0) {
            paneList
            Rectangle().fill(room.hairline).frame(width: 1)
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 6) {
                    MonoCaps("Settings", size: 10)
                    Text(pane.title)
                        .font(.displayHero(32))
                        .foregroundStyle(room.ink)
                        .accessibilityAddTraits(.isHeader)
                }
                .padding(.horizontal, 28)
                .padding(.top, 36)
                Form {
                    switch pane {
                    case .account: accountPane
                    case .appearance: appearancePane
                    case .playback: playbackPane
                    case .audio: audioPane
                    case .notifications: notificationsPane
                    case .privacy: privacyPane
                    case .updates: updatesPane
                    case .daemon: daemonPane
                    case .about: aboutPane
                    }
                }
                .formStyle(.grouped)
                // Let the room show through; the grouped sections keep their
                // native controls, which are the OS's job.
                .scrollContentBackground(.hidden)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(width: 760, height: 560)
        .toolbar(removing: .title)
        .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        .background { RoomFloor(imageURL: model.player.currentItem?.imageURL) }
        .environment(\.colorScheme, room.isLight ? .light : .dark)
        .task {
            await model.config.load()
            await model.config.loadAudioOutputs()
        }
        .overlay(alignment: .bottom) {
            if let err = model.config.errorMessage {
                Text(err).font(.caption).foregroundStyle(ConnectionDot.down)
                    .padding(.horizontal, 12).padding(.vertical, 8)
                    .background(room.raised, in: Capsule())
                    .overlay(Capsule().strokeBorder(room.hairline))
                    .padding(.bottom, 10)
            }
        }
    }

    /// The pane column, set like the main sidebar.
    private var paneList: some View {
        VStack(alignment: .leading, spacing: 1) {
            MonoCaps("Preferences", size: 9.5)
                .padding(.horizontal, 12)
                .padding(.bottom, 8)
            ForEach(Pane.allCases) { p in
                SettingsPaneRow(title: p.title, icon: p.icon, isSelected: p == pane) { pane = p }
            }
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.top, 44)
        .frame(width: 200)
    }

    // MARK: Binding helpers (config keys -> daemon config via the CLI)

    private func text(_ key: String) -> Binding<String> {
        Binding(get: { model.config.string(key) }, set: { model.config.set(key, $0) })
    }
    private func toggle(_ key: String) -> Binding<Bool> {
        Binding(get: { model.config.bool(key) }, set: { model.config.setBool(key, $0) })
    }
    private func intText(_ key: String) -> Binding<String> {
        Binding(
            get: { model.config.string(key) },
            set: { model.config.set(key, String(Int($0.filter(\.isNumber)) ?? 0)) })
    }

    // MARK: Panes

    @ViewBuilder private var appearancePane: some View {
        Section("Theme") {
            Picker("Appearance", selection: desktopTheme) {
                ForEach(AppTheme.allCases) { theme in
                    Label(theme.label, systemImage: theme.systemImage).tag(theme)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
        }
        Section {
            Text("Adaptive lights a dark room with colour from the playing record, whatever the system appearance. Light gives a warm paper room, Dark a neutral dark one, and Follow System picks between those two.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var accountPane: some View {
        Section("Spotify app") {
            LabeledContent("Client ID") { TextField("", text: text("client_id")).textFieldStyle(.roundedBorder) }
            LabeledContent("Client secret") { SecretField(store: model.config) }
            LabeledContent("Redirect URI") { TextField("", text: text("redirect_uri")).textFieldStyle(.roundedBorder) }
        }
        Text("Create an app at the Spotify Developer Dashboard with redirect URI `http://127.0.0.1:8888/callback`. A secret is optional for PKCE.")
            .font(.caption).foregroundStyle(.secondary)
    }

    @ViewBuilder private var playbackPane: some View {
        Section("Player") {
            TextField("Backend", text: text("player.backend"))
            Picker("Bitrate", selection: text("player.bitrate")) {
                Text("96 kbps").tag("96"); Text("160 kbps").tag("160"); Text("320 kbps").tag("320")
            }
            TextField("Device name", text: text("player.device_name"))
            Toggle("Volume normalization", isOn: toggle("player.normalization"))
            LabeledContent("Audio cache (MiB)") {
                TextField("", text: intText("player.audio_cache_mib")).frame(width: 90).textFieldStyle(.roundedBorder)
            }
            TextField("Event hook command", text: text("player.event_hook"))
        }
    }

    @ViewBuilder private var audioPane: some View {
        Section("Output device") {
            Picker("Output", selection: text("player.audio_output_device")) {
                Text("System default").tag("")
                ForEach(model.config.audioOutputs, id: \.self) { Text($0).tag($0) }
            }
            if model.config.audioOutputs.isEmpty {
                Text("No devices enumerated yet.").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var notificationsPane: some View {
        Section {
            Toggle("Enable notifications", isOn: toggle("notifications.enabled"))
        }
        Section("Notify on") {
            Toggle("Track change", isOn: toggle("notifications.on_track_change"))
            Toggle("Pause", isOn: toggle("notifications.on_pause"))
            Toggle("Resume", isOn: toggle("notifications.on_resume"))
            Toggle("Skip", isOn: toggle("notifications.on_skip"))
            Toggle("Error", isOn: toggle("notifications.on_error"))
        }
        Section("Templates") {
            TextField("Summary", text: text("notifications.summary"))
            TextField("Body", text: text("notifications.body"))
        }
    }

    @ViewBuilder private var privacyPane: some View {
        Section("Analytics") {
            TextField("Listen hook command", text: text("analytics.hook_command"))
            LabeledContent("Hook timeout (ms)") {
                TextField("", text: intText("analytics.hook_timeout_ms")).frame(width: 90).textFieldStyle(.roundedBorder)
            }
        }
        Section("Cover cache") {
            LabeledContent("Max size (MB)") {
                TextField("", text: intText("cache.cover_cache_mb")).frame(width: 90).textFieldStyle(.roundedBorder)
            }
            LabeledContent("TTL (days)") {
                TextField("", text: intText("cache.cover_cache_ttl_days")).frame(width: 90).textFieldStyle(.roundedBorder)
            }
        }
    }

    @ViewBuilder private var updatesPane: some View {
        UpdatesPaneBody()
    }

    @ViewBuilder private var daemonPane: some View {
        Section("Connection") {
            LabeledContent("Status") {
                HStack(spacing: 6) {
                    ConnectionDot(health: statusHealth, size: 8)
                    Text(statusText)
                }
            }
            LabeledContent("Socket") {
                Text(model.socketPath).font(.caption.monospaced()).textSelection(.enabled).foregroundStyle(.secondary)
            }
            Button("Reconnect") { model.forceReconnect() }
            Button("Open config file") { openConfigFile() }
        }
    }

    @ViewBuilder private var aboutPane: some View {
        Section {
            LabeledContent("Spotuify", value: "macOS client")
            LabeledContent("Version", value: appVersion)
            Link("Releases", destination: URL(string: "https://github.com/planetaryescape/spotuify/releases")!)
        }
        Text("A native player for the spotuify daemon — the same daemon the CLI and TUI drive. Playback runs in the daemon; this app is a view.")
            .font(.caption).foregroundStyle(.secondary)
    }

    // MARK: Helpers

    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown"
    }

    private func openConfigFile() {
        Task {
            if let path = try? await CLIRunner.run(["config", "path"]) {
                let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: trimmed)])
                }
            }
        }
    }

    private var statusHealth: ConnectionDot.Health {
        switch model.connectionState {
        case .ready: .ok
        case .connecting, .reconnecting, .idle: .working
        case .failed: .down
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

/// Updates pane body — split out so it can hold the auto-check `@AppStorage`.
private struct UpdatesPaneBody: View {
    @Environment(AppModel.self) private var model
    @AppStorage("autoCheckUpdates") private var autoCheckUpdates = true

    var body: some View {
        Section {
            LabeledContent("This app", value: model.appVersion.isEmpty ? "unknown" : model.appVersion)
            Toggle("Check for updates automatically", isOn: $autoCheckUpdates)
            Button("Check Now") { model.checkUpdate(force: true) }
        }
        if let update = model.availableUpdate {
            Section("Available") {
                LabeledContent("Latest", value: update.latestVersion).foregroundStyle(.tint)
                switch model.updater.phase {
                case .downloading, .verifying, .installing:
                    LabeledContent("Status") {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text(updaterStatus).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                case .installed(let url):
                    Button("Relaunch to finish update") { AppRelaunch.relaunch(from: url) }
                case .failed(let message):
                    Text(message).font(.caption).foregroundStyle(ConnectionDot.down)
                    Button("Retry") {
                        model.updater.reset()
                        model.installAvailableUpdate()
                    }
                    if let url = update.url, let u = URL(string: url) {
                        Button("Open releases page") { NSWorkspace.shared.open(u) }
                    }
                case .idle:
                    Button("Update Now") { model.installAvailableUpdate() }
                    if let command = update.command {
                        LabeledContent("Or via terminal") {
                            Text(command).font(.caption.monospaced()).textSelection(.enabled)
                        }
                    }
                }
            }
        } else {
            Text("You're up to date.").font(.caption).foregroundStyle(.secondary)
        }
    }

    private var updaterStatus: String {
        switch model.updater.phase {
        case .downloading: "Downloading…"
        case .verifying: "Verifying…"
        case .installing: "Installing…"
        default: ""
        }
    }
}

/// Secret field that shows empty (never the real/redacted secret) and only
/// writes when the user actually types a new value.
private struct SecretField: View {
    let store: ConfigStore
    @State private var entry = ""

    var body: some View {
        SecureField(store.isRedactedSecret("client_secret") ? "•••••• (set)" : "", text: $entry)
            .textFieldStyle(.roundedBorder)
            .onSubmit { commit() }
            .onChange(of: entry) { _, _ in } // hold; commit on submit/blur
    }

    private func commit() {
        let trimmed = entry.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { store.set("client_secret", trimmed); entry = "" }
    }
}

/// One pane in the Settings column: ink when current, with the accent tick.
private struct SettingsPaneRow: View {
    @Environment(\.room) private var room
    let title: String
    let icon: String
    let isSelected: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(isSelected ? room.accent : room.inkFaint)
                    .frame(width: 18)
                Text(title)
                    .font(.system(size: 13, weight: isSelected ? .semibold : .medium))
                    .foregroundStyle(isSelected ? room.ink : room.inkMuted)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .frame(height: 30)
            .background(
                RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
                    .fill(room.ink.opacity(isSelected ? 0.08 : (hovering ? 0.04 : 0))))
            .overlay(alignment: .leading) {
                Capsule().fill(room.accent).frame(width: 3, height: isSelected ? 14 : 0).offset(x: -1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }
}
