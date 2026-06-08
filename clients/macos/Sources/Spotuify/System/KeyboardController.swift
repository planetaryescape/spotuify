import AppKit
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
@MainActor
final class KeyboardController {
    static let shared = KeyboardController()

    private weak var model: AppModel?
    private var monitor: Any?

    private init() {}

    /// Install the local key monitor once.
    func configure(model: AppModel) {
        self.model = model
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            self?.handle(event) ?? event
        }
    }

    /// Return `nil` to swallow the event, or the event itself to let it through.
    private func handle(_ event: NSEvent) -> NSEvent? {
        guard let model, !isEditingText else { return event }
        // Only bare chords — leave ⌘/⌥/⌃ combinations to the menu shortcuts so
        // we never shadow ⌘1…⌘9 navigation or ⌘← / ⌘→ seek.
        let mods = event.modifierFlags
            .intersection(.deviceIndependentFlagsMask)
            .subtracting(.capsLock)
        guard mods.isEmpty else { return event }

        switch event.keyCode {
        case 49: // Space → play / pause
            model.togglePlayPause()
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
}
