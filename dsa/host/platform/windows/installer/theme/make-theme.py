# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""Draws the images of setup.exe's theme into this folder.

    uv run dsa/host/platform/windows/installer/theme/make-theme.py

The look follows WinUI: a light backdrop, a white card with rounded corners and a thin
border, and buttons with 4 px corners. The colours are Android's: the accent button is
Android green with dark navy text. The logo is an app tile with two windows on it, one per
Android task; it uses Android's colours but no Android mark.

WiX draws a button's image and then its text, so the images carry no words and the setup
text stays translatable. The window size and the button sizes here must match theme.xml.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

HERE = Path(__file__).resolve().parent

WINDOW = (640, 440)
CARD = (24, 112, 616, 372)  # left, top, right, bottom
GREEN = (61, 220, 132)
NAVY = (7, 48, 66)
# Buttons: name -> (width, height). Must match the Button sizes in theme.xml.
BUTTONS = {"104": (104, 32), "200": (200, 32)}
SCALE = 4  # draw large and shrink, for smooth edges


def smooth(size, draw_fn):
    """Draws at SCALE times the size and shrinks, which anti-aliases the edges."""
    w, h = size
    big = Image.new("RGBA", (w * SCALE, h * SCALE), (0, 0, 0, 0))
    draw_fn(ImageDraw.Draw(big), SCALE)
    return big.resize(size, Image.LANCZOS)


def rounded(draw, box, radius, fill, outline=None, width=1, s=1):
    x0, y0, x1, y1 = (v * s for v in box)
    draw.rounded_rectangle((x0, y0, x1 - 1, y1 - 1), radius * s, fill=fill,
                           outline=outline, width=width * s if outline else 0)


def background():
    w, h = WINDOW
    image = Image.new("RGB", WINDOW)
    top, bottom = (248, 250, 249), (236, 241, 238)
    for y in range(h):
        t = y / (h - 1)
        image.paste(tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)), (0, y, w, y + 1))
    # Soft colour, as Mica tints the backdrop: green from the top right, navy from the
    # bottom left.
    glow = Image.new("RGBA", WINDOW, (0, 0, 0, 0))
    d = ImageDraw.Draw(glow)
    d.ellipse((w - 330, -230, w + 170, 190), fill=GREEN + (70,))
    d.ellipse((-200, h - 150, 180, h + 210), fill=NAVY + (22,))
    image = Image.alpha_composite(image.convert("RGBA"), glow.filter(ImageFilter.GaussianBlur(70)))
    # The card's shadow, then the card.
    shadow = Image.new("RGBA", WINDOW, (0, 0, 0, 0))
    x0, y0, x1, y1 = CARD
    ImageDraw.Draw(shadow).rounded_rectangle((x0, y0 + 3, x1, y1 + 4), 10, fill=(0, 0, 0, 34))
    image = Image.alpha_composite(image, shadow.filter(ImageFilter.GaussianBlur(7)))
    card = smooth(WINDOW, lambda d, s: rounded(d, CARD, 8, (255, 255, 255, 255),
                                                outline=(224, 229, 227, 255), s=s))
    image = Image.alpha_composite(image, card)
    image.convert("RGB").save(HERE / "background.png", optimize=True)


def button(size, fill, outline, accent_line=None):
    w, h = size

    def draw(d, s):
        rounded(d, (0, 0, w, h), 4, fill, outline=outline, s=s)
        if accent_line:  # WinUI's focus line: a darker stroke along the bottom
            d.rounded_rectangle((1 * s, (h - 2) * s, (w - 1) * s - 1, h * s - 1), 1 * s,
                                fill=accent_line)

    return smooth(size, draw)


def buttons():
    states = {
        # Accent: Android green, darker as it is pressed.
        "primary": {
            "normal": ((61, 220, 132, 255), (48, 196, 114, 255), None),
            "hover": ((52, 204, 120, 255), (42, 180, 104, 255), None),
            "pressed": ((43, 182, 106, 255), (36, 160, 92, 255), None),
            "focus": ((61, 220, 132, 255), (7, 48, 66, 255), None),
        },
        # Standard: white with a thin border, as WinUI's default button.
        "secondary": {
            "normal": ((255, 255, 255, 255), (209, 214, 212, 255), (196, 201, 199, 255)),
            "hover": ((246, 248, 247, 255), (204, 209, 207, 255), (190, 195, 193, 255)),
            "pressed": ((239, 242, 241, 255), (214, 219, 217, 255), None),
            "focus": ((255, 255, 255, 255), (7, 48, 66, 255), None),
        },
    }
    for kind, by_state in states.items():
        # The wide button only ever comes as a standard one (Update setup first).
        for name, size in BUTTONS.items():
            if kind == "primary" and name != "104":
                continue
            for state, (fill, outline, line) in by_state.items():
                button(size, fill, outline, line).save(HERE / f"button-{kind}-{name}-{state}.png")


def logo(size):
    def draw(d, s):
        n = size
        # The app tile: Android green, rounded like a Windows 11 app icon.
        rounded(d, (0, 0, n, n), n * 0.22, GREEN + (255,), s=s)
        # Two windows, the back one navy, the front one white with a navy title bar.
        def window(x0, y0, x1, y1, body, bar):
            rounded(d, (x0, y0, x1, y1), n * 0.06, body, s=s)
            d.rounded_rectangle((x0 * s, y0 * s, x1 * s - 1, (y0 + n * 0.11) * s), n * 0.06 * s,
                                fill=bar)
        window(n * 0.16, n * 0.20, n * 0.66, n * 0.62, NAVY + (255,), (14, 74, 98, 255))
        # A ring of the tile's green keeps the front window apart from the back one.
        gap = n * 0.035
        rounded(d, (n * 0.34 - gap, n * 0.38 - gap, n * 0.84 + gap, n * 0.80 + gap),
                n * 0.09, GREEN + (255,), s=s)
        window(n * 0.34, n * 0.38, n * 0.84, n * 0.80, (255, 255, 255, 255), NAVY + (255,))

    return smooth((size, size), draw)


def logos():
    logo(64).save(HERE / "logo.png")
    big = logo(256)
    big.save(HERE / "logo.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)])


if __name__ == "__main__":
    background()
    buttons()
    logos()
    print(f"wrote the theme images to {HERE}")
