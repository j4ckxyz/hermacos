// Draws the app icon and writes app/Resources/AppIcon.icns.
//   swift scripts/make-icon.swift
import AppKit

let size: CGFloat = 1024
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let context = NSGraphicsContext.current!.cgContext

// macOS icon grid: an 824 pt rounded square centred on a 1024 pt canvas.
let plate = CGRect(x: 100, y: 100, width: 824, height: 824)
let shape = NSBezierPath(roundedRect: plate, xRadius: 186, yRadius: 186)

context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: NSColor.black.withAlphaComponent(0.32).cgColor)
NSColor.black.setFill()
shape.fill()
context.restoreGState()

context.saveGState()
shape.addClip()
let background = NSGradient(colors: [
    NSColor(srgbRed: 0.24, green: 0.15, blue: 0.05, alpha: 1),
    NSColor(srgbRed: 0.08, green: 0.05, blue: 0.02, alpha: 1),
])!
background.draw(in: plate, angle: -90)
// A soft amber glow behind the mark.
let glow = NSGradient(colors: [
    NSColor(srgbRed: 1.0, green: 0.67, blue: 0.05, alpha: 0.42),
    NSColor(srgbRed: 1.0, green: 0.67, blue: 0.05, alpha: 0),
])!
glow.draw(fromCenter: CGPoint(x: 512, y: 500), radius: 0, toCenter: CGPoint(x: 512, y: 500), radius: 430, options: [])
context.restoreGState()

// The mark: the same symbol the app uses, in an amber gradient.
let configuration = NSImage.SymbolConfiguration(pointSize: 430, weight: .bold)
let symbol = NSImage(systemSymbolName: "bolt.horizontal.fill", accessibilityDescription: nil)!
    .withSymbolConfiguration(configuration)!
let markSize = symbol.size
let markRect = CGRect(x: (size - markSize.width) / 2, y: (size - markSize.height) / 2, width: markSize.width, height: markSize.height)
let mark = NSImage(size: markSize, flipped: false) { rect in
    symbol.draw(in: rect)
    NSGraphicsContext.current!.cgContext.setBlendMode(.sourceIn)
    NSGradient(colors: [
        NSColor(srgbRed: 1.0, green: 0.82, blue: 0.36, alpha: 1),
        NSColor(srgbRed: 1.0, green: 0.56, blue: 0.0, alpha: 1),
    ])!.draw(in: rect, angle: -90)
    return true
}
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -8), blur: 22, color: NSColor.black.withAlphaComponent(0.45).cgColor)
mark.draw(in: markRect)
context.restoreGState()

// Hairline highlight along the plate's edge.
NSColor.white.withAlphaComponent(0.12).setStroke()
let rim = NSBezierPath(roundedRect: plate.insetBy(dx: 1.5, dy: 1.5), xRadius: 184.5, yRadius: 184.5)
rim.lineWidth = 3
rim.stroke()

NSGraphicsContext.current = nil
let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "icon-1024.png")
try rep.representation(using: .png, properties: [:])!.write(to: output)
