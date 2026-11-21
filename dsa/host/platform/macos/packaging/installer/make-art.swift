// Draws the installer's artwork from the Windows setup design: the DSA logo
// (dsa/host/platform/windows/installer/setup-ui/assets/logo.png) and the setup window's
// accent green (0F7B4A). Run it again after the logo changes:
//
//   swift make-art.swift ../../../windows/installer/setup-ui/assets/logo.png resources dmg
//
// It writes background.png and background-dark.png for the Installer's sidebar (drawn at
// its bottom left, unscaled), and dmg-background.png with dmg-background@2x.png for the disk
// image window, which build-app.sh joins into one TIFF.
import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count == 4, let logo = NSImage(contentsOfFile: args[1])?
    .cgImage(forProposedRect: nil, context: nil, hints: nil)
else {
    FileHandle.standardError.write("usage: make-art.swift <logo.png> <installer folder> <disk image folder>\n".data(using: .utf8)!)
    exit(2)
}
var out = URL(fileURLWithPath: args[2])
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let accent = (r: 0x0F / 255.0, g: 0x7B / 255.0, b: 0x4A / 255.0)

func write(_ name: String, width: Int, height: Int, draw: (CGContext) -> Void) {
    let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                        space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.interpolationQuality = .high
    draw(ctx)
    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    try! rep.representation(using: .png, properties: [:])!.write(to: out.appendingPathComponent(name))
}

// The sidebar: transparent, a soft accent glow in the corner and the logo on it. The
// Installer window is 620 by 418 points; the sidebar is about the left 170.
func sidebar(_ name: String, glow: Double) {
    write(name, width: 170, height: 418) { ctx in
        let gradient = CGGradient(colorsSpace: space, colors: [
            CGColor(srgbRed: accent.r, green: accent.g, blue: accent.b, alpha: glow),
            CGColor(srgbRed: accent.r, green: accent.g, blue: accent.b, alpha: 0),
        ] as CFArray, locations: [0, 1])!
        ctx.drawRadialGradient(gradient, startCenter: CGPoint(x: 60, y: 60), startRadius: 0,
                               endCenter: CGPoint(x: 60, y: 60), endRadius: 200, options: [])
        ctx.draw(logo, in: CGRect(x: 20, y: 20, width: 96, height: 96))
    }
}
sidebar("background.png", glow: 0.18)
sidebar("background-dark.png", glow: 0.35)

// The disk image window, 600 by 400 points: DSA.app on the left, Applications on the right
// (make-dmg-store.swift puts their centres 190 points from the top), an arrow between them
// and the logo faint behind.
func dmg(_ name: String, scale: Int) {
    let s = CGFloat(scale)
    write(name, width: 600 * scale, height: 400 * scale) { ctx in
        ctx.scaleBy(x: s, y: s)
        let gradient = CGGradient(colorsSpace: space, colors: [
            CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1),
            CGColor(srgbRed: 0.90, green: 0.95, blue: 0.92, alpha: 1),
        ] as CFArray, locations: [0, 1])!
        ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: 400), end: CGPoint(x: 0, y: 0), options: [])
        ctx.setAlpha(0.06)
        ctx.draw(logo, in: CGRect(x: 220, y: 130, width: 160, height: 160))
        ctx.setAlpha(1)
        ctx.setStrokeColor(CGColor(srgbRed: accent.r, green: accent.g, blue: accent.b, alpha: 1))
        ctx.setLineWidth(4)
        ctx.setLineCap(.round)
        ctx.setLineJoin(.round)
        ctx.move(to: CGPoint(x: 245, y: 210))
        ctx.addLine(to: CGPoint(x: 355, y: 210))
        ctx.move(to: CGPoint(x: 335, y: 226))
        ctx.addLine(to: CGPoint(x: 355, y: 210))
        ctx.addLine(to: CGPoint(x: 335, y: 194))
        ctx.strokePath()
    }
}
out = URL(fileURLWithPath: args[3])
dmg("dmg-background.png", scale: 1)
dmg("dmg-background@2x.png", scale: 2)
