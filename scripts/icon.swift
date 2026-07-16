// Generates the app icon: a rounded-square gradient tile with a white "K".
// Usage: swiftc scripts/icon.swift -o /tmp/kimi-icon && /tmp/kimi-icon icons/icon.png
import AppKit

let canvas: CGFloat = 1024
let image = NSImage(size: NSSize(width: canvas, height: canvas))
image.lockFocus()

// Tile: modern macOS icon geometry (~80% of canvas, generous corner radius).
let inset: CGFloat = 112
let tile = NSRect(x: inset, y: inset, width: canvas - inset * 2, height: canvas - inset * 2)
let path = NSBezierPath(roundedRect: tile, xRadius: 190, yRadius: 190)
let gradient = NSGradient(colors: [
    NSColor(calibratedRed: 0.07, green: 0.12, blue: 0.32, alpha: 1),
    NSColor(calibratedRed: 0.16, green: 0.50, blue: 0.93, alpha: 1),
])!
gradient.draw(in: path, angle: -50)

// Letter.
let letter = "K" as NSString
let font = NSFont.systemFont(ofSize: 520, weight: .bold)
let attrs: [NSAttributedString.Key: Any] = [
    .font: font,
    .foregroundColor: NSColor.white,
]
let letterSize = letter.size(withAttributes: attrs)
let origin = NSPoint(
    x: tile.midX - letterSize.width / 2,
    y: tile.midY - letterSize.height / 2
)
letter.draw(at: origin, withAttributes: attrs)

image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:])
else { fatalError("png encode failed") }

let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icons/icon.png"
try! png.write(to: URL(fileURLWithPath: out))
print("icon written to \(out)")
