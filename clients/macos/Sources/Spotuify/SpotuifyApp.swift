import SwiftUI
import SpotuifyKit

@main
struct SpotuifyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model = AppModel()
    @State private var theme = ArtworkTheme()
    @State private var navigator = Navigator()

    var body: some Scene {
        // `Window` (not `WindowGroup`) so there is exactly one player window —
        // re-invoking `openWindow(id: "player")` focuses the existing one
        // instead of spawning a new window each time.
        Window("Spotuify", id: "player") {
            RootView()
                .environment(model)
                .environment(theme)
                .environment(navigator)
                .task {
                    // Self-contained install: drop the bundled daemon+CLI onto
                    // the user's PATH so the backend is available everywhere.
                    DaemonLauncher.installBundledCLIIfNeeded()
                    model.start()
                    SystemMediaController.shared.configure(model: model)
                    KeyboardController.shared.configure(model: model)
                    ReminderNotificationScheduler.shared.configure(model: model)
                }
                .onChange(of: model.player.playback) { _, _ in
                    Task { await SystemMediaController.shared.updateNowPlaying(player: model.player) }
                }
                // The displayed track can also change via a queue update (which
                // doesn't touch `playback`), and play/pause must refresh the
                // Now Playing state — republish on both.
                .onChange(of: model.player.currentItem?.uri) { _, _ in
                    Task { await SystemMediaController.shared.updateNowPlaying(player: model.player) }
                }
                .onChange(of: model.player.isPlaying) { _, _ in
                    Task { await SystemMediaController.shared.updateNowPlaying(player: model.player) }
                }
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 980, height: 720)
        .commands {
            CommandGroup(after: .appSettings) {
                CheckForUpdatesCommand(model: model)
            }
            CommandGroup(after: .windowArrangement) {
                MiniPlayerCommand()
            }
            CommandMenu("Playback") { PlaybackCommands(model: model) }
            CommandMenu("Go") { GoCommands(navigator: navigator) }
        }

        // Single floating HUD window — likewise reused, never duplicated.
        Window("Mini Player", id: "mini-player") {
            MiniPlayerView()
                .environment(model)
                .environment(theme)
                .task { model.start() }
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 320, height: 380)

        MenuBarExtra("Spotuify", systemImage: "music.note") {
            MenuBarView()
                .environment(model)
                .environment(theme)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environment(model)
        }
    }
}

/// Gates the player UI behind a daemon presence + version check.
struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.readiness {
        case .ready:
            AppShell()
        default:
            DaemonGateView(readiness: model.readiness)
        }
    }
}

/// Global playback keyboard control (Space / ⌘arrows / ⌘⇧S / ⌘⇧R), shown in
/// the Playback menu so the shortcuts are discoverable. Space play/pause yields
/// to a focused text field (it inserts a space there instead).
private struct PlaybackCommands: View {
    let model: AppModel
    var body: some View {
        Button("Play / Pause") { model.togglePlayPause() }
            .keyboardShortcut(.space, modifiers: [])
        Button("Next") { model.next() }
            .keyboardShortcut(.rightArrow, modifiers: .command)
        Button("Previous") { model.previous() }
            .keyboardShortcut(.leftArrow, modifiers: .command)
        Divider()
        Button("Volume Up") { model.setVolume(Int(model.player.volumePercent ?? 0) + 5) }
            .keyboardShortcut(.upArrow, modifiers: .command)
        Button("Volume Down") { model.setVolume(Int(model.player.volumePercent ?? 0) - 5) }
            .keyboardShortcut(.downArrow, modifiers: .command)
        Divider()
        Button("Toggle Shuffle") { model.toggleShuffle() }
            .keyboardShortcut("s", modifiers: [.command, .shift])
        Button("Cycle Repeat") { model.cycleRepeat() }
            .keyboardShortcut("r", modifiers: [.command, .shift])
    }
}

/// View navigation: ⌘1…⌘9, ⌘0 jump to each destination (mirrors the TUI's
/// 1–9/0 and the sidebar order).
private struct GoCommands: View {
    let navigator: Navigator
    var body: some View {
        ForEach(Array(Navigator.numbered.enumerated()), id: \.element.id) { index, dest in
            Button(dest.title) { navigator.selection = dest }
                .keyboardShortcut(
                    KeyEquivalent(Character("\((index + 1) % 10)")), modifiers: .command)
        }
    }
}

/// "Check for Updates…" in the app menu — forces a fresh check and opens
/// Settings so the result (Updates pane + banner) is visible.
private struct CheckForUpdatesCommand: View {
    let model: AppModel
    @Environment(\.openSettings) private var openSettings
    var body: some View {
        Button("Check for Updates…") {
            model.checkUpdate(force: true)
            openSettings()
        }
    }
}

/// Menu command + ⌘⇧M shortcut to open the floating mini-player.
private struct MiniPlayerCommand: View {
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        Button("Mini Player") { openWindow(id: "mini-player") }
            .keyboardShortcut("m", modifiers: [.command, .shift])
    }
}
