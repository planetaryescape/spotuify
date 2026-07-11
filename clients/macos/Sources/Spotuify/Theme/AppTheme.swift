import SwiftUI

/// The user's chosen desktop appearance, persisted via `@AppStorage("desktopTheme")`.
///
/// `adaptive` is the historical default: the accent/flood follow the album
/// artwork (see `ArtworkTheme`). The others pin a fixed base `ColorScheme` and
/// use a polished fixed palette instead of the cover flood.
public enum AppTheme: String, CaseIterable, Identifiable {
    case system, light, dark, adaptive

    public var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "Follow System"
        case .light: "Light"
        case .dark: "Dark"
        case .adaptive: "Adaptive"
        }
    }

    var systemImage: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .light: "sun.max"
        case .dark: "moon"
        case .adaptive: "paintpalette"
        }
    }

    /// The forced base scheme. `nil` = follow the OS. `adaptive` follows the OS
    /// base scheme too; its accent/flood are layered on top from the artwork.
    var preferredColorScheme: ColorScheme? {
        switch self {
        case .light: .light
        case .dark: .dark
        case .system, .adaptive: nil
        }
    }

    /// Whether cover-art-derived colors drive the UI (the classic behavior).
    var isAdaptive: Bool { self == .adaptive }
}
