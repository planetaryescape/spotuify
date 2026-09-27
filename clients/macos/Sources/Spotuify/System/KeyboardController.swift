import AppKit
import Observation
import SpotuifyKit

/// Bare-key playback shortcuts that must beat the responder chain. A SwiftUI
/// menu shortcut for an unmodified Space almost never fires: a focused button,
/// list, or scroll view consumes the space first (activate / page-down), so the
/// key never reaches the menu. A local event monitor sees the key *before* the
/// responder chain and routes it to the daemon — except while a text field is
/// being edited, where Space must still type a space (search boxes, etc.).
///
/// This mirrors Spotify/Apple Music: Space is global play/pause everywhere but
/// the search field. ⌘-modified shortcuts stay in the Playback menu, where
/// command chords reach menu items reliably.
///
/// It also gives the custom sidebar a native list's keyboard behaviour. SwiftUI
/// `.focusable()` is gated on Full Keyboard Access on macOS and never took
/// focus, so the monitor keeps a small focus model instead: a click in the
/// sidebar focuses it, a click elsewhere in the player window unfocuses it, and
/// ↑/↓ move the selection while it's focused.
@MainActor
@Observable
final class KeyboardController {
    static let shared = KeyboardController()

    /// Whether ↑/↓ drive the sidebar. True at launch, like a fresh sidebar.
    private(set) var sidebarHasFocus = true
    /// Set by the sidebar: moves the selection by ±1.
    @ObservationIgnored var moveSidebarSelection: ((Int) -> Void)?

    @ObservationIgnored private weak var model: AppModel?
    @ObservationIgnored private var monitor: Any?

    private init() {}

    /// Install the local key monitor once.
    func configure(model: AppModel) {
        self.model = model
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .leftMouseDown]) { [weak self] event in
            self?.handle(event) ?? event
        }
    }

    /// Return `nil` to swallow the event, or the event itself to let it through.
    private func handle(_ event: NSEvent) -> NSEvent? {
        if event.type == .leftMouseDown {
            if Self.isPlayerWindow(event.window) {
                let sidebarVisible = UserDefaults.standard.object(forKey: "sidebarVisible") as? Bool ?? true
                let focused = sidebarVisible && event.locationInWindow.x <= Theme.sidebarWidth
                // Write only on change: an observable write re-renders the
                // sidebar between mouse-down and mouse-up of the same click.
                if focused != sidebarHasFocus { sidebarHasFocus = focused }
            }
            return event
        }
        guard let model, !isEditingText else { return event }
        // Only bare chords — leave ⌘/⌥/⌃ combinations to the menu shortcuts so
        // we never shadow ⌘1…⌘9 navigation or ⌘← / ⌘→ seek.
        // Arrow keys always carry `.function` and `.numericPad`; they aren't
        // chords, so they don't count as modifiers here.
        let mods = event.modifierFlags
            .intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .function, .numericPad])
        guard mods.isEmpty else { return event }

        switch event.keyCode {
        case 49: // Space → play / pause
            model.togglePlayPause()
            return nil
        case 125, 126: // ↓ / ↑ → sidebar selection, while the sidebar has focus
            guard sidebarHasFocus, Self.isPlayerWindow(event.window),
                  let move = moveSidebarSelection else { return event }
            move(event.keyCode == 125 ? 1 : -1)
            return nil
        default:
            return event
        }
    }

    /// True while a text field / search box is first responder, so a typed space
    /// reaches it instead of toggling playback. SwiftUI text editing is backed by
    /// the window's field editor — an `NSText` (NSTextView) subclass.
    private var isEditingText: Bool {
        NSApp.keyWindow?.firstResponder is NSText
    }

    /// The main window (SwiftUI `Window(id: "player")`), not the mini player,
    /// Settings, or a sheet.
    private static func isPlayerWindow(_ window: NSWindow?) -> Bool {
        window?.identifier?.rawValue.hasPrefix("player") == true
    }
}
