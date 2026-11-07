import AppKit

/// The menu bar item's icon: a template image (one colour on a clear background), so the
/// system colours it for a light or a dark menu bar. It is the DSA symbol, one app window
/// with two antennae and its inner panel left open.
///
/// DSA.app carries the image as `MenuBarTemplate.png` and `MenuBarTemplate@2x.png`, rendered
/// from `dsa/brand/menubar-template.svg`. A copy of the app run outside its bundle draws the
/// same shape here instead.
enum MenuBarIcon {
    static let template: NSImage = {
        let image = Bundle.main.image(forResource: "MenuBarTemplate") ?? drawn()
        image.size = NSSize(width: 18, height: 18)
        image.isTemplate = true
        image.accessibilityDescription = "DSA"
        return image
    }()

    /// The symbol on the 64 unit grid of its master SVG, scaled to 18 points.
    private static func drawn() -> NSImage {
        NSImage(size: NSSize(width: 18, height: 18), flipped: true) { _ in
            let scale = NSAffineTransform()
            scale.scale(by: 18.0 / 64.0)
            scale.concat()
            NSColor.black.setStroke()
            NSColor.black.setFill()
            let antennae = NSBezierPath()
            antennae.move(to: NSPoint(x: 22, y: 14))
            antennae.line(to: NSPoint(x: 16.5, y: 5))
            antennae.move(to: NSPoint(x: 42, y: 14))
            antennae.line(to: NSPoint(x: 47.5, y: 5))
            antennae.lineWidth = 4.4
            antennae.lineCapStyle = .round
            antennae.stroke()
            let body = NSBezierPath(roundedRect: NSRect(x: 4, y: 12, width: 56, height: 46), xRadius: 9, yRadius: 9)
            body.append(NSBezierPath(roundedRect: NSRect(x: 12, y: 32, width: 40, height: 18), xRadius: 4, yRadius: 4))
            body.windingRule = .evenOdd
            body.fill()
            return true
        }
    }
}
