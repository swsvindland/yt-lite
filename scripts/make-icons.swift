#!/usr/bin/env swift
// Draws the yt-lite app icon (the sidebar logo: a white play glyph on brand
// red) and writes it in each platform's format:
//
//   ios/YtLite/Assets.xcassets/AppIcon.appiconset/AppIcon.png   full-bleed square
//   crates/desktop/resources/macos/AppIcon.icns                  rounded, with margins
//   crates/desktop/resources/windows/yt-lite.ico                 rounded, 16–256 px
//
// The Android launcher icon is a vector drawable with the same play path
// (android/app/src/main/res/drawable/ic_launcher_foreground.xml).
//
//     swift scripts/make-icons.swift
import AppKit
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let red = CGColor(srgbRed: 1, green: 0, blue: 0x33 / 255.0, alpha: 1)
let white = CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1)

/// Lucide's "play" (24-unit box, as in the desktop sidebar), filled: a
/// triangle with corner radius 2. Vertices are where its edges meet.
func playPath(in box: CGRect) -> CGPath {
    let s = box.width / 24
    // y grows downward in the icon's 24-unit box; flip into CoreGraphics.
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: box.minX + x * s, y: box.maxY - y * s) }
    let top = p(5, 1.517), tip = p(22.97, 12), bottom = p(5, 22.483)
    let path = CGMutablePath()
    path.move(to: p(5, 12))
    path.addArc(tangent1End: top, tangent2End: tip, radius: 2 * s)
    path.addArc(tangent1End: tip, tangent2End: bottom, radius: 2 * s)
    path.addArc(tangent1End: bottom, tangent2End: top, radius: 2 * s)
    path.closeSubpath()
    return path
}

enum Shape {
    /// iOS: the system masks the corners.
    case fullBleed
    /// A rounded square inset by `margin` (fraction of the size), corner
    /// radius as a fraction of the square.
    case rounded(margin: CGFloat, radius: CGFloat, shadow: Bool)
}

func render(size: Int, shape: Shape) -> CGImage {
    let alpha: CGImageAlphaInfo = { if case .fullBleed = shape { return .noneSkipLast } else { return .premultipliedLast } }()
    let ctx = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: alpha.rawValue
    )!
    let full = CGRect(x: 0, y: 0, width: size, height: size)
    var body = full
    switch shape {
    case .fullBleed:
        ctx.setFillColor(red)
        ctx.fill(full)
    case let .rounded(margin, radius, shadow):
        body = full.insetBy(dx: CGFloat(size) * margin, dy: CGFloat(size) * margin)
        let r = body.width * radius
        let rounded = CGPath(roundedRect: body, cornerWidth: r, cornerHeight: r, transform: nil)
        ctx.saveGState()
        if shadow {
            ctx.setShadow(
                offset: CGSize(width: 0, height: -CGFloat(size) * 0.01), blur: CGFloat(size) * 0.025,
                color: CGColor(gray: 0, alpha: 0.3)
            )
        }
        ctx.addPath(rounded)
        ctx.setFillColor(red)
        ctx.fillPath()
        ctx.restoreGState()
    }
    // The glyph's 24-unit box spans 52% of the icon body (Lucide centers it optically).
    let side = body.width * 0.52
    let box = CGRect(x: body.midX - side / 2, y: body.midY - side / 2, width: side, height: side)
    ctx.addPath(playPath(in: box))
    ctx.setFillColor(white)
    ctx.fillPath()
    return ctx.makeImage()!
}

func pngData(_ image: CGImage) -> Data {
    let data = NSMutableData()
    let dest = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, image, nil)
    CGImageDestinationFinalize(dest)
    return data as Data
}

func write(_ data: Data, _ path: String) throws {
    let url = URL(fileURLWithPath: path)
    try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try data.write(to: url)
    print("wrote \(path)")
}

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().path
FileManager.default.changeCurrentDirectoryPath(root)

// iOS: one 1024 px opaque icon; Xcode derives the other sizes.
let catalog = "ios/YtLite/Assets.xcassets"
try write(Data(#"{ "info" : { "author" : "xcode", "version" : 1 } }"#.utf8), "\(catalog)/Contents.json")
try write(Data("""
{
  "images" : [
    { "filename" : "AppIcon.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
""".utf8), "\(catalog)/AppIcon.appiconset/Contents.json")
try write(pngData(render(size: 1024, shape: .fullBleed)), "\(catalog)/AppIcon.appiconset/AppIcon.png")

// macOS: Apple's icon grid puts an 824 px rounded square on a 1024 canvas.
let mac = Shape.rounded(margin: 100.0 / 1024, radius: 185.4 / 824, shadow: true)
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
for base in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(base)x\(base).png" : "icon_\(base)x\(base)@2x.png"
        try write(pngData(render(size: base * scale, shape: mac)), iconset.appendingPathComponent(name).path)
    }
}
let icns = "crates/desktop/resources/macos/AppIcon.icns"
try FileManager.default.createDirectory(atPath: "crates/desktop/resources/macos", withIntermediateDirectories: true)
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", icns]
try iconutil.run()
iconutil.waitUntilExit()
guard iconutil.terminationStatus == 0 else { fatalError("iconutil failed") }
print("wrote \(icns)")

// Windows: an .ico with PNG-compressed entries (supported since Vista).
let sizes = [16, 24, 32, 48, 64, 128, 256]
let windows = Shape.rounded(margin: 1.0 / 32, radius: 0.22, shadow: false)
let pngs = sizes.map { pngData(render(size: $0, shape: windows)) }
var ico = Data()
func le16(_ v: Int) { ico.append(contentsOf: [UInt8(v & 0xff), UInt8(v >> 8 & 0xff)]) }
func le32(_ v: Int) { le16(v & 0xffff); le16(v >> 16) }
le16(0); le16(1); le16(sizes.count)
var offset = 6 + 16 * sizes.count
for (size, png) in zip(sizes, pngs) {
    ico.append(UInt8(size == 256 ? 0 : size)); ico.append(UInt8(size == 256 ? 0 : size))
    ico.append(0); ico.append(0)  // palette, reserved
    le16(1); le16(32)  // planes, bits per pixel
    le32(png.count); le32(offset)
    offset += png.count
}
pngs.forEach { ico.append($0) }
try write(ico, "crates/desktop/resources/windows/yt-lite.ico")
