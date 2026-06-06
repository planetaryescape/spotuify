import SwiftUI
import CoreText

/// Registers the bundled Fraunces display cuts (SIL OFL, vendored under
/// `Resources/Fonts`) and exposes the editorial type scale. Body and UI text
/// stay on the system font; Fraunces is display-only — characterful serif
/// headlines over a neutral sans body is the magazine formula that reads bold
/// without getting noisy.
enum EditorialFont {
    static let black = "Fraunces72pt-Black"
    static let semibold = "Fraunces72pt-SemiBold"
    static let lightItalic = "Fraunces72pt-LightItalic"

    private static let fileNames = [black, semibold, lightItalic]
    @MainActor private static var didRegister = false

    /// Idempotent; call once at launch before the first view renders.
    @MainActor static func register() {
        guard !didRegister else { return }
        didRegister = true
        let urls = fileNames.compactMap {
            Bundle.main.url(forResource: $0, withExtension: "ttf")
        } as CFArray
        CTFontManagerRegisterFontURLs(urls, .process, false, nil)
    }
}

extension Font {
    /// Big editorial hero titles (track / album names on the now-playing stage).
    static func displayHero(_ size: CGFloat) -> Font { .custom(EditorialFont.black, size: size) }
    /// Section and secondary display headings.
    static func displayTitle(_ size: CGFloat) -> Font { .custom(EditorialFont.semibold, size: size) }
    /// Editorial accent — light italic for eyebrows, pull quotes, numerals.
    static func displayAccent(_ size: CGFloat) -> Font { .custom(EditorialFont.lightItalic, size: size) }
}
