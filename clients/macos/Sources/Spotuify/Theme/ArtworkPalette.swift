import AppKit
import SwiftUI

/// A role-based color palette derived from album artwork.
///
/// Replaces the old single `areaAverage` accent (which mushed the whole cover
/// into one muddy mean). A coarse popularity histogram yields the *dominant*
/// color for the background flood plus the most *colorful prominent* swatch for
/// a vivid accent, then text colors are chosen for guaranteed contrast.
struct ArtworkPalette: Equatable, Sendable {
    /// Deep, hue-matched base for the full-bleed flood behind the hero.
    var background: Color
    /// Title text — chosen light/dark for legibility over `background`.
    var primary: Color
    /// Metadata / secondary text.
    var secondary: Color
    /// Vivid accent for controls, seek fill, and glass tint.
    var accent: Color
    /// Whether `background` is light (drives text + glass variant choices).
    var isLight: Bool

    /// Polished fixed DARK palette — the historical adaptive fallback, reused as
    /// the base for the Dark / System(dark) fixed themes.
    static let darkFallback = ArtworkPalette(
        background: Color(white: 0.13),
        primary: .white,
        secondary: Color(white: 0.72),
        accent: .accentColor,
        isLight: false)

    /// Polished fixed LIGHT palette for the Light / System(light) fixed themes:
    /// a near-white surface with near-black text and the app AccentColor.
    static let lightFallback = ArtworkPalette(
        background: Color(white: 0.98),
        primary: Color(white: 0.10),
        secondary: Color(white: 0.42),
        accent: .accentColor,
        isLight: true)
}

extension ArtworkPalette {
    static func extract(from image: NSImage) -> ArtworkPalette? {
        guard let swatches = Swatch.histogram(from: image), !swatches.isEmpty else { return nil }

        // Dominant by population → the flood/background source.
        let dominant = swatches.max { $0.weight < $1.weight }!
        let isLight = dominant.luminance > 0.6
        let background = dominant.asBackground()

        // Accent: most "colorful and present" swatch (frequency × saturation ×
        // brightness), ignoring near-grays and the dark/bright extremes. For a
        // monochrome cover, fall back to a clean neutral rather than fake a hue.
        let colorful = swatches.filter { $0.saturation > 0.25 && $0.brightness > 0.2 }
        let accentNS = (colorful.max { $0.colorfulScore < $1.colorfulScore })?.vivid()
            ?? (isLight ? NSColor(white: 0.15, alpha: 1) : NSColor(white: 0.92, alpha: 1))

        let primary: Color = isLight ? Color(white: 0.08) : .white
        let secondary: Color = isLight ? Color(white: 0.30) : Color(white: 0.78)

        return ArtworkPalette(
            background: Color(nsColor: background),
            primary: primary,
            secondary: secondary,
            accent: Color(nsColor: accentNS.contrasting(against: background)),
            isLight: isLight)
    }
}

/// One quantized color bucket from the artwork histogram.
private struct Swatch {
    let color: NSColor
    let weight: Int
    let hue: CGFloat
    let saturation: CGFloat
    let brightness: CGFloat

    var luminance: CGFloat { color.relativeLuminance }
    var colorfulScore: Double {
        Double(weight) * Double(saturation) * (0.4 + Double(brightness) * 0.6)
    }

    /// Punch the swatch up into a usable UI accent.
    func vivid() -> NSColor {
        NSColor(
            hue: hue,
            saturation: min(1, max(0.6, saturation * 1.35)),
            brightness: min(0.98, max(0.62, brightness * 1.15)),
            alpha: 1)
    }

    /// Keep the dominant hue/character but pull to a legible depth for text.
    func asBackground() -> NSColor {
        NSColor(
            hue: hue,
            saturation: min(0.75, max(0.22, saturation * 0.9)),
            brightness: brightness > 0.6 ? 0.30 : max(0.12, brightness * 0.7),
            alpha: 1)
    }

    /// Downscale to 48×48 and bucket pixels into a coarse popularity histogram.
    static func histogram(from image: NSImage) -> [Swatch]? {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        let w = 48, h = 48
        var data = [UInt8](repeating: 0, count: w * h * 4)
        guard let ctx = CGContext(
            data: &data, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))

        var bins: [Int: (count: Int, r: Int, g: Int, b: Int)] = [:]
        bins.reserveCapacity(128)
        var i = 0
        while i < data.count {
            if data[i + 3] > 16 { // skip near-transparent
                let r = Int(data[i]), g = Int(data[i + 1]), b = Int(data[i + 2])
                let key = (r / 52) * 25 + (g / 52) * 5 + (b / 52) // 5 levels/channel
                var bin = bins[key] ?? (0, 0, 0, 0)
                bin.count += 1; bin.r += r; bin.g += g; bin.b += b
                bins[key] = bin
            }
            i += 4
        }

        return bins.values.map { bin in
            let color = NSColor(
                srgbRed: CGFloat(bin.r / bin.count) / 255,
                green: CGFloat(bin.g / bin.count) / 255,
                blue: CGFloat(bin.b / bin.count) / 255,
                alpha: 1)
            var hue: CGFloat = 0, sat: CGFloat = 0, bri: CGFloat = 0, alpha: CGFloat = 0
            color.getHue(&hue, saturation: &sat, brightness: &bri, alpha: &alpha)
            return Swatch(color: color, weight: bin.count, hue: hue, saturation: sat, brightness: bri)
        }
    }
}

private extension NSColor {
    /// WCAG relative luminance in sRGB.
    var relativeLuminance: CGFloat {
        guard let c = usingColorSpace(.sRGB) else { return 0 }
        func lin(_ v: CGFloat) -> CGFloat {
            v <= 0.03928 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * lin(c.redComponent) + 0.7152 * lin(c.greenComponent) + 0.0722 * lin(c.blueComponent)
    }

    func contrastRatio(to other: NSColor) -> CGFloat {
        let hi = max(relativeLuminance, other.relativeLuminance)
        let lo = min(relativeLuminance, other.relativeLuminance)
        return (hi + 0.05) / (lo + 0.05)
    }

    /// Nudge brightness until the color stands off `background` by `ratio`.
    func contrasting(against background: NSColor, ratio: CGFloat = 2.4) -> NSColor {
        let bgIsDark = background.relativeLuminance < 0.4
        var result = self
        var steps = 0
        while result.contrastRatio(to: background) < ratio && steps < 8 {
            var h: CGFloat = 0, s: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
            (result.usingColorSpace(.sRGB) ?? result).getHue(&h, saturation: &s, brightness: &b, alpha: &a)
            b = bgIsDark ? min(1, b + 0.08) : max(0, b - 0.08)
            result = NSColor(hue: h, saturation: s, brightness: b, alpha: 1)
            steps += 1
        }
        return result
    }
}
