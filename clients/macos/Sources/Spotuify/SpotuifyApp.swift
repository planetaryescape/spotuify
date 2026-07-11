import AppKit
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
                .desktopTheme(theme)
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
                .desktopTheme(theme)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 320, height: 380)

        MenuBarExtra("Spotuify", systemImage: "music.note") {
            MenuBarView()
                .environment(model)
                .environment(theme)
                .desktopTheme(theme)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environment(model)
                .desktopTheme(theme)
        }
    }
}

/// Gates the player UI behind a daemon presence + version check.
struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
            switch model.readiness {
            case .ready:
                AppShell()
            default:
                DaemonGateView(readiness: model.readiness)
            }
        }
        // Viz focus is a per-client vote on the daemon; report ours so
        // an unfocused TUI can't throttle this app's visualizer (and
        // vice versa). Also vote on connect, since a stale vote from a
        // previous run may still be on file.
        .onReceive(NotificationCenter.default.publisher(
            for: NSApplication.didBecomeActiveNotification
        )) { _ in
            model.setVizFocus(true)
        }
        .onReceive(NotificationCenter.default.publisher(
            for: NSApplication.willResignActiveNotification
        )) { _ in
            model.setVizFocus(false)
        }
        .onChange(of: model.isReady) { _, ready in
            if ready { model.setVizFocus(NSApp.isActive) }
        }
        // Window closed (app keeps running via the menu bar): nothing
        // shows the visualizer anymore, so withdraw our focused vote —
        // a stale `true` pinned the daemon's spectrum broadcast at
        // full rate into a windowless app indefinitely.
        .onDisappear {
            model.setVizFocus(false)
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

/// View navigation: existing destinations keep ⌘1…⌘9, ⌘0; Notifications uses
/// ⌘⇧0 because macOS only has ten numeric keys.
private struct GoCommands: View {
    let navigator: Navigator
    var body: some View {
        ForEach(Array(Navigator.numbered.enumerated()), id: \.element.id) { index, dest in
            Button(dest.title) { navigator.selection = dest }
                .keyboardShortcut(
                    KeyEquivalent(Character(index < 10 ? "\((index + 1) % 10)" : "0")),
                    modifiers: index < 10 ? .command : [.command, .shift])
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

/// Applies the user's desktop theme (`@AppStorage("desktopTheme")`) to a scene
/// root. Forces the base `ColorScheme` via `.preferredColorScheme`, and — since
/// the mini-player and menubar are SEPARATE scenes that don't inherit it — also
/// drives `NSApp.appearance` app-wide as belt-and-suspenders. Keeps
/// `ArtworkTheme` in sync: adaptive re-extracts from artwork; fixed themes get a
/// polished fixed palette for the resolved light/dark scheme.
private struct DesktopThemeModifier: ViewModifier {
    let theme: ArtworkTheme
    @AppStorage("desktopTheme") private var themeRaw = AppTheme.adaptive.rawValue
    @Environment(\.colorScheme) private var systemScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var appTheme: AppTheme { AppTheme(rawValue: themeRaw) ?? .adaptive }

    func body(content: Content) -> some View {
        content
            .preferredColorScheme(appTheme.preferredColorScheme)
            .onAppear { apply() }
            .onChange(of: themeRaw) { _, _ in apply() }
            .onChange(of: systemScheme) { _, _ in apply() }
    }

    @MainActor
    private func apply() {
        switch appTheme {
        case .light: NSApp.appearance = NSAppearance(named: .aqua)
        case .dark: NSApp.appearance = NSAppearance(named: .darkAqua)
        case .system, .adaptive: NSApp.appearance = nil
        }
        theme.adaptiveEnabled = appTheme.isAdaptive
        if !appTheme.isAdaptive {
            // Resolve `.system` against the live OS scheme; `.light`/`.dark` are
            // explicit. The artwork `.task` repopulates when adaptive re-enables.
            theme.applyFixed(appTheme.preferredColorScheme ?? systemScheme, reduceMotion: reduceMotion)
        }
    }
}

extension View {
    /// Apply the persisted desktop theme to a scene root. Pass the shared
    /// `ArtworkTheme` so fixed themes can swap to a fixed palette.
    func desktopTheme(_ theme: ArtworkTheme) -> some View {
        modifier(DesktopThemeModifier(theme: theme))
    }
}
