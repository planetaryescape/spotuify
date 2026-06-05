import AppKit
import SwiftUI
import CoreImage
import CoreImage.CIFilterBuiltins

/// Derives a vivid accent color from album artwork using CoreImage's area
/// average, then boosts saturation/brightness so it works as a UI tint.
enum DominantColor {
    private static let context = CIContext(options: [.workingColorSpace: NSNull()])

    static func accent(from image: NSImage) -> Color? {
        palette(from: image)?.accent
    }

    /// Extract a vivid accent plus a darkened background tint from artwork.
    static func palette(from image: NSImage) -> (accent: Color, background: Color)? {
        guard let tiff = image.tiffRepresentation,
              let ciImage = CIImage(data: tiff) else { return nil }

        let filter = CIFilter.areaAverage()
        filter.inputImage = ciImage
        filter.extent = ciImage.extent
        guard let output = filter.outputImage else { return nil }

        var bitmap = [UInt8](repeating: 0, count: 4)
        context.render(
            output,
            toBitmap: &bitmap,
            rowBytes: 4,
            bounds: CGRect(x: 0, y: 0, width: 1, height: 1),
            format: .RGBA8,
            colorSpace: CGColorSpaceCreateDeviceRGB())

        let base = NSColor(
            srgbRed: CGFloat(bitmap[0]) / 255,
            green: CGFloat(bitmap[1]) / 255,
            blue: CGFloat(bitmap[2]) / 255,
            alpha: 1)

        var hue: CGFloat = 0, saturation: CGFloat = 0, brightness: CGFloat = 0, alpha: CGFloat = 0
        base.usingColorSpace(.sRGB)?.getHue(&hue, saturation: &saturation, brightness: &brightness, alpha: &alpha)

        // Vivid accent for controls/tint.
        let accent = NSColor(
            hue: hue,
            saturation: min(1, max(0.55, saturation * 1.4)),
            brightness: min(0.95, max(0.6, brightness * 1.25)),
            alpha: 1)
        // Darkened, lightly saturated background tint that stays legible.
        let background = NSColor(
            hue: hue,
            saturation: min(0.7, max(0.3, saturation)),
            brightness: 0.22,
            alpha: 1)
        return (Color(nsColor: accent), Color(nsColor: background))
    }
}
