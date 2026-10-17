# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""Draws the images of setup.exe's theme into this folder.

    uv run dsa/host/platform/windows/installer/theme/make-theme.py

The layout is a WinUI dialog with a brand panel: an Android navy panel on the left with the
logo and the product name, a white page on the right, and a light footer bar that holds the
buttons, as in a WinUI ContentDialog. Android green is the one accent colour.

WiX draws the window from one backdrop image and draws each button as an image with its text
on top. The engine never erases behind a button, so a button image with transparent corners
would blend over itself on every hover and darken. Every button image is therefore opaque,
with the backdrop it sits on baked into its corners and into the 3 px margin that holds the
focus ring. That ties each image to one surface: the footer, the page, or the brand panel.

The images carry no words, so the setup text stays translatable. The sizes and positions
here must match theme.xml.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

HERE = Path(__file__).resolve().parent

# Window geometry. theme.xml uses the same numbers.
WINDOW = (736, 440)
PANEL_W = 256  # the brand panel on the left
FOOTER_TOP = 368  # the footer bar runs from here to the bottom, right of the panel

# Palette. Android's brand navy and green, and WinUI's neutral surfaces.
NAVY = (7, 48, 66)  # 073042
PANEL_TOP = (10, 58, 80)  # 0A3A50, the panel's gradient, lighter at the top
PANEL_BOTTOM = (5, 38, 53)  # 052635
GREEN = (61, 220, 132)  # 3DDC84
GREEN_DEEP = (16, 140, 74)  # 108C4A, green that holds 3:1 on white for the progress bar
GREEN_TRACK = (217, 240, 227)  # D9F0E3
PAGE = (255, 255, 255)
FOOTER = (243, 245, 244)  # F3F5F4
DIVIDER = (224, 229, 226)  # E0E5E2
INK = (28, 43, 51)  # 1C2B33, body text
FOCUS = (14, 30, 38)  # 0E1E26, WinUI's focus ring is near black
CRITICAL = (196, 43, 28)  # C42B1C, WinUI's critical colour

# Buttons: the visible button is 32 px high with 4 px corners. The image is 3 px larger on
# each side so the focus ring fits inside the control.
PAD = 3
STANDARD = (112, 32)
WIDE = (192, 32)
UPDATE_POS = (32, 332)  # top left of the visible update button on the brand panel
SCALE = 4  # draw large and shrink, for smooth edges


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def rgba(c, a=255):
    return tuple(c) + (a,)


def luminance(c):
    def ch(v):
        v /= 255
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (ch(v) for v in c)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def smooth(size, draw_fn):
    """Draws at SCALE times the size and shrinks, which anti-aliases the edges."""
    w, h = size
    big = Image.new("RGBA", (w * SCALE, h * SCALE), (0, 0, 0, 0))
    draw_fn(ImageDraw.Draw(big), SCALE)
    return big.resize(size, Image.LANCZOS)


def rrect(d, box, radius, s, fill=None, outline=None, width=1):
    x0, y0, x1, y1 = (v * s for v in box)
    d.rounded_rectangle((x0, y0, x1 - 1, y1 - 1), radius * s, fill=fill, outline=outline,
                        width=round(width * s) if outline else 0)


def vertical_gradient(size, top, bottom):
    w, h = size
    image = Image.new("RGB", size)
    for y in range(h):
        image.paste(mix(top, bottom, y / max(h - 1, 1)), (0, y, w, y + 1))
    return image


# The backdrop.

def motif(size):
    """Three app windows stacked at the panel's lower right, each a little nearer than the
    last: the product in one picture. Drawn faint so the panel stays quiet."""
    w, h = size

    def draw(d, s):
        windows = [
            # x0, y0, x1, y1, stroke alpha, fill alpha. They run off the panel's right edge
            # and stay clear of the version text at the lower left.
            (192, 228, 372, 340, 40, 0),
            (160, 272, 340, 384, 64, 10),
            (128, 316, 308, 428, 104, 22),
        ]
        for x0, y0, x1, y1, line, fill in windows:
            # Knock out what is behind, so the windows read as stacked, not as a wireframe.
            rrect(d, (x0, y0, x1, y1), 10, s, fill=rgba(mix(PANEL_TOP, PANEL_BOTTOM, y0 / h)))
            if fill:
                rrect(d, (x0, y0, x1, y1), 10, s, fill=rgba(GREEN, fill))
            rrect(d, (x0, y0, x1, y1), 10, s, outline=rgba(GREEN, line), width=1.25)
            # The title bar: a line under it and the app's icon at its left.
            d.line((x0 * s, (y0 + 22) * s, x1 * s, (y0 + 22) * s), fill=rgba(GREEN, line),
                   width=round(1.25 * s))
            rrect(d, (x0 + 9, y0 + 7, x0 + 17, y0 + 15), 2.5, s, fill=rgba(GREEN, line))
            d.line(((x0 + 23) * s, (y0 + 11) * s, (x0 + 55) * s, (y0 + 11) * s),
                   fill=rgba(GREEN, line // 2), width=round(2 * s))

    return smooth(size, draw)


def background():
    w, h = WINDOW
    image = Image.new("RGBA", WINDOW, rgba(PAGE))
    panel = vertical_gradient((PANEL_W, h), PANEL_TOP, PANEL_BOTTOM).convert("RGBA")
    # A soft green glow behind the logo, the way Mica picks up colour.
    glow = Image.new("RGBA", (PANEL_W, h), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse((-120, -140, 200, 150), fill=rgba(GREEN, 34))
    panel = Image.alpha_composite(panel, glow.filter(ImageFilter.GaussianBlur(60)))
    panel = Image.alpha_composite(panel, motif((PANEL_W, h)))
    # The logo's shadow, under the ImageControl that theme.xml places at 32,32.
    shadow = Image.new("RGBA", (PANEL_W, h), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((33, 36, 79, 82), 12, fill=(0, 0, 0, 90))
    panel = Image.alpha_composite(panel, shadow.filter(ImageFilter.GaussianBlur(5)))
    image.paste(panel, (0, 0))
    # The footer bar and the hairline above it.
    d = ImageDraw.Draw(image)
    d.rectangle((PANEL_W, FOOTER_TOP, w - 1, h - 1), fill=rgba(FOOTER))
    d.line((PANEL_W, FOOTER_TOP, w - 1, FOOTER_TOP), fill=rgba(DIVIDER))
    image = image.convert("RGB")
    image.save(HERE / "background.png", optimize=True)
    return image


# Buttons.

def button(size, surface, fill, outline, bottom=None, ring=None, ring_inner=None):
    """One button state on its surface. `surface` is an RGB image the size of the control."""
    w, h = size

    def draw(d, s):
        if ring:  # WinUI's focus visual: a 2 px outer ring and a 1 px inner ring
            rrect(d, (0, 0, w, h), 4 + PAD, s, outline=rgba(ring), width=2)
            rrect(d, (2, 2, w - 2, h - 2), 4 + 1, s, outline=rgba(ring_inner), width=1)
        body = (PAD, PAD, w - PAD, h - PAD)
        rrect(d, body, 4, s, fill=fill)
        rrect(d, body, 4, s, outline=outline, width=1)
        if bottom:  # the darker bottom edge that gives WinUI buttons their lift
            x0, y0, x1, y1 = body
            d.line(((x0 + 4) * s, (y1 - 0.5) * s, (x1 - 4) * s, (y1 - 0.5) * s), fill=bottom,
                   width=s)

    layer = smooth(size, draw)
    return Image.alpha_composite(surface.convert("RGBA"), layer).convert("RGB")


def buttons(backdrop):
    W, H = STANDARD[0] + 2 * PAD, STANDARD[1] + 2 * PAD
    footer = Image.new("RGB", (W, H), FOOTER)
    page = Image.new("RGB", (W, H), PAGE)

    # Accent: Android green with navy text. Hover and pressed darken it with a navy state
    # layer, as Material does, rather than WinUI's lighter accent.
    g_hover, g_pressed = mix(GREEN, NAVY, 0.08), mix(GREEN, NAVY, 0.16)
    accent = {
        "normal": (rgba(GREEN), rgba(mix(GREEN, NAVY, 0.12)), rgba(mix(GREEN, NAVY, 0.34)), None),
        "hover": (rgba(g_hover), rgba(mix(g_hover, NAVY, 0.12)), rgba(mix(g_hover, NAVY, 0.34)), None),
        "pressed": (rgba(g_pressed), rgba(mix(g_pressed, NAVY, 0.10)), None, None),
        "focus": (rgba(GREEN), rgba(mix(GREEN, NAVY, 0.12)), rgba(mix(GREEN, NAVY, 0.34)), FOCUS),
    }
    # Standard: near white with a hairline border and a darker bottom edge.
    standard = {
        "normal": (rgba((253, 253, 253)), rgba((219, 224, 221)), rgba((196, 202, 199)), None),
        "hover": (rgba((247, 249, 248)), rgba((214, 219, 216)), rgba((192, 198, 195)), None),
        "pressed": (rgba((240, 243, 241)), rgba((219, 224, 221)), None, None),
        "focus": (rgba((253, 253, 253)), rgba((219, 224, 221)), rgba((196, 202, 199)), FOCUS),
    }
    for state, (fill, outline, bottom, ring) in accent.items():
        button((W, H), footer, fill, outline, bottom, ring, PAGE).save(
            HERE / f"button-accent-{state}.png")
    for state, (fill, outline, bottom, ring) in standard.items():
        button((W, H), footer, fill, outline, bottom, ring, PAGE).save(
            HERE / f"button-standard-{state}.png")
        button((W, H), page, fill, outline, bottom, ring, PAGE).save(
            HERE / f"button-browse-{state}.png")

    # Update setup first: an outlined green button on the brand panel. It is filled solid so
    # the panel's motif does not show through it, and its corners hold the panel's own
    # pixels, cut from the backdrop where theme.xml places it.
    w, h = WIDE[0] + 2 * PAD, WIDE[1] + 2 * PAD
    x, y = UPDATE_POS[0] - PAD, UPDATE_POS[1] - PAD
    under = backdrop.crop((x, y, x + w, y + h))
    update = {
        "normal": (rgba(mix(PANEL_BOTTOM, GREEN, 0.06)), rgba(GREEN, 230), None, None),
        "hover": (rgba(mix(PANEL_BOTTOM, GREEN, 0.14)), rgba(GREEN), None, None),
        "pressed": (rgba(mix(PANEL_BOTTOM, GREEN, 0.02)), rgba(GREEN, 170), None, None),
        "focus": (rgba(mix(PANEL_BOTTOM, GREEN, 0.06)), rgba(GREEN, 230), None, (255, 255, 255)),
    }
    for state, (fill, outline, bottom, ring) in update.items():
        button((w, h), under, fill, outline, bottom, ring, NAVY).save(
            HERE / f"button-update-{state}.png")


def progressbar():
    """The engine stretches a 4 px wide strip: column 0 is the left end, 1 the filled part,
    2 the empty part, 3 the right end. A flat 4 px bar on a tinted track, as Material's
    linear progress indicator."""
    image = Image.new("RGB", (4, 4))
    for x, colour in enumerate((GREEN_DEEP, GREEN_DEEP, GREEN_TRACK, GREEN_TRACK)):
        image.paste(colour, (x, 0, x + 1, 4))
    image.save(HERE / "progressbar.png")


# The logo: a green app tile with two windows on it, one per Android task. It is this
# project's own mark and uses no Android or Microsoft symbol.

def logo(size):
    n = size
    simple = n <= 20  # at 16 and 20 px two windows turn to mush; draw one
    big = n * SCALE

    # The tile: Android green, a touch lighter at the top, as a Windows 11 app icon is lit.
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, big - 1, big - 1), n * 0.24 * SCALE, fill=255)
    top = GREEN if simple else mix(GREEN, (255, 255, 255), 0.14)
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    image.paste(vertical_gradient((big, big), top, GREEN), (0, 0), mask)
    d = ImageDraw.Draw(image)
    s = SCALE

    def window(x0, y0, x1, y1, body, bar, radius):
        rrect(d, (x0, y0, x1, y1), radius, s, fill=rgba(body))
        bar_h = n * 0.12
        d.rounded_rectangle((x0 * s, y0 * s, x1 * s - 1, (y0 + bar_h) * s), radius * s,
                            fill=rgba(bar))
        d.rectangle((x0 * s, (y0 + bar_h / 2) * s, x1 * s - 1, (y0 + bar_h) * s), fill=rgba(bar))

    if simple:
        window(n * 0.17, n * 0.22, n * 0.83, n * 0.78, PAGE, NAVY, n * 0.08)
    else:
        window(n * 0.15, n * 0.19, n * 0.65, n * 0.59, NAVY, (18, 84, 110), n * 0.06)
        # A ring of the tile's green parts the front window from the back one.
        gap = n * 0.045
        rrect(d, (n * 0.35 - gap, n * 0.41 - gap, n * 0.85 + gap, n * 0.81 + gap),
              n * 0.06 + gap, s, fill=rgba(mix(GREEN, top, 0.4)))
        window(n * 0.35, n * 0.41, n * 0.85, n * 0.81, PAGE, NAVY, n * 0.06)
    return image.resize((n, n), Image.LANCZOS)


def logos():
    # Drawn at twice its 48 px place in theme.xml, so it stays sharp at 200% scaling.
    logo(96).save(HERE / "logo.png")
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [logo(n) for n in sizes]
    images[-1].save(HERE / "logo.ico", sizes=[(n, n) for n in sizes], append_images=images[:-1])


def status_icons():
    """The badges above the heading on the Success and Failure pages, at 2x their 40 px."""
    n = 80

    def success(d, s):
        d.ellipse((0, 0, n * s - 1, n * s - 1), fill=rgba(GREEN))
        w = round(7 * s)
        pts = [(23 * s, 41 * s), (35 * s, 53 * s), (58 * s, 29 * s)]
        d.line(pts, fill=rgba(NAVY), width=w, joint="curve")
        for x, y in (pts[0], pts[-1]):
            d.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=rgba(NAVY))

    def failure(d, s):
        d.ellipse((0, 0, n * s - 1, n * s - 1), fill=rgba(CRITICAL))
        d.rounded_rectangle((36 * s, 18 * s, 44 * s, 48 * s), 4 * s, fill=rgba(PAGE))
        d.ellipse((35.5 * s, 54 * s, 44.5 * s, 63 * s), fill=rgba(PAGE))

    smooth((n, n), success).save(HERE / "status-success.png")
    smooth((n, n), failure).save(HERE / "status-failure.png")


def check_contrast():
    """WCAG AA: 4.5:1 for text, 3:1 for the progress bar. Fails loudly if a colour drifts."""
    pairs = {
        "body on page": (INK, PAGE, 4.5),
        "heading on page": (NAVY, PAGE, 4.5),
        "secondary on page": ((86, 101, 109), PAGE, 4.5),
        "button text on accent": (NAVY, GREEN, 4.5),
        "button text on accent pressed": (NAVY, mix(GREEN, NAVY, 0.16), 4.5),
        "standard text on button": (INK, (240, 243, 241), 4.5),
        "title on panel": ((255, 255, 255), PANEL_TOP, 4.5),
        "tagline on panel": ((181, 202, 211), PANEL_TOP, 4.5),
        "version on panel": ((143, 169, 180), PANEL_TOP, 4.5),
        "update text on panel": (GREEN, PANEL_TOP, 4.5),
        "progress fill on page": (GREEN_DEEP, PAGE, 3.0),
    }
    for name, (fg, bg, need) in pairs.items():
        ratio = contrast(fg, bg)
        assert ratio >= need, f"{name}: {ratio:.2f}:1 is under {need}:1"


if __name__ == "__main__":
    check_contrast()
    backdrop = background()
    buttons(backdrop)
    progressbar()
    logos()
    status_icons()
    print(f"wrote the theme images to {HERE}")
