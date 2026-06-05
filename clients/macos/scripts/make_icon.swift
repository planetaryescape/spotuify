#!/usr/bin/env swift
import AppKit
import CoreGraphics

// Renders the Spotuify app icon: a violet squircle with a white "equalizer"
// motif (matching the in-app spectrum visualizer). Writes pixel-accurate PNGs
// and the AppIcon.appiconset Contents.json. Run:
//   swift scripts/make_icon.swift Sources/Spotuify/Assets.xcassets/AppIcon.appiconset

let outDir = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "Sources/Spotuify/Assets.xcassets/AppIcon.appiconset"

func render(_ px: Int) -> Data {
    let s = CGFloat(px)
    guard let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else {
        fatalError("rep")
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext

    ctx.clear(CGRect(x: 0, y: 0, width: s, height: s))

    // Squircle body
    let margin = s * 0.085
    let rect = CGRect(x: margin, y: margin, width: s - 2 * margin, height: s - 2 * margin)
    let radius = rect.width * 0.2237
    let body = CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
    ctx.saveGState()
    ctx.addPath(body)
    ctx.clip()
    let space = CGColorSpaceCreateDeviceRGB()
    let gradient = CGGradient(
        colorsSpace: space,
        colors: [
            CGColor(red: 0.58, green: 0.39, blue: 0.98, alpha: 1),
            CGColor(red: 0.36, green: 0.19, blue: 0.69, alpha: 1),
        ] as CFArray,
        locations: [0, 1])!
    ctx.drawLinearGradient(
        gradient,
        start: CGPoint(x: rect.minX, y: rect.maxY),
        end: CGPoint(x: rect.maxX, y: rect.minY),
        options: [])
    ctx.restoreGState()

    // Equalizer bars
    let barCount = 4
    let heights: [CGFloat] = [0.36, 0.66, 0.48, 0.78]
    let area = rect.insetBy(dx: rect.width * 0.27, dy: rect.height * 0.20)
    let gap = area.width * 0.12
    let barWidth = (area.width - gap * CGFloat(barCount - 1)) / CGFloat(barCount)
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.96))
    for index in 0..<barCount {
        let height = area.height * heights[index]
        let x = area.minX + CGFloat(index) * (barWidth + gap)
        let barRect = CGRect(x: x, y: area.minY, width: barWidth, height: height)
        let corner = barWidth * 0.45
        ctx.addPath(CGPath(roundedRect: barRect, cornerWidth: corner, cornerHeight: corner, transform: nil))
        ctx.fillPath()
    }

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let uniqueSizes = [16, 32, 64, 128, 256, 512, 1024]
for px in uniqueSizes {
    let data = render(px)
    let path = "\(outDir)/icon_\(px).png"
    try! data.write(to: URL(fileURLWithPath: path))
    print("wrote \(path)")
}

// Map (size pt, scale) -> pixel file
let entries: [(Int, Int)] = [
    (16, 1), (16, 2), (32, 1), (32, 2),
    (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2),
]
var images = "["
images += entries.map { size, scale in
    let px = size * scale
    return """

    {
      "size" : "\(size)x\(size)",
      "idiom" : "mac",
      "filename" : "icon_\(px).png",
      "scale" : "\(scale)x"
    }
"""
}.joined(separator: ",")
images += "\n  ]"

let contents = """
{
  "images" : \(images),
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
"""
try! contents.write(to: URL(fileURLWithPath: "\(outDir)/Contents.json"), atomically: true, encoding: .utf8)
print("wrote \(outDir)/Contents.json")
