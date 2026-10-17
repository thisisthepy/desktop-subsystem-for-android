# Writes the setup logo: logo.png (256 px) and logo.ico (16 to 256 px).
#
#   uv run make-logo.py logo.png logo.ico
#
# The picture is two overlapping window frames, one per task, on a rounded green tile. The
# green is only a colour; the logo carries no mark of anyone else. It is drawn on a 96 unit
# grid and supersampled, so every size is drawn from the shapes rather than scaled.
import struct, sys, zlib

GRID = 96.0
TILE_TOP = (0x5A, 0xE5, 0x97)
TILE_BOTTOM = (0x3E, 0xDC, 0x85)
RING = (0x48, 0xDE, 0x8B)
BACK_BAR = (0x13, 0x56, 0x6E)
DARK = (0x07, 0x30, 0x42)
WHITE = (0xFF, 0xFF, 0xFF)


def rounded(x, y, x0, y0, x1, y1, r):
    """Whether (x, y) is inside the rectangle x0..x1, y0..y1 with corners of radius r."""
    if x < x0 or x >= x1 or y < y0 or y >= y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def colour(x, y):
    """The colour at grid point (x, y), or None outside the tile."""
    if not rounded(x, y, 0, 0, 96, 96, 22):
        return None
    t = y / GRID
    c = tuple(round(a + (b - a) * t) for a, b in zip(TILE_TOP, TILE_BOTTOM))
    # The back window: a lighter title bar over a dark body.
    if rounded(x, y, 15, 18, 62, 57, 5):
        c = BACK_BAR if y < 30 else DARK
    # A ring in the tile colour separates the front window from the back one.
    if rounded(x, y, 29, 34, 86, 82, 10):
        c = RING
    # The front window: a dark title bar over a white body.
    if rounded(x, y, 34, 39, 81, 77, 5):
        c = DARK if y < 51 else WHITE
    return c


def render(size, samples=4):
    rows = []
    step = GRID / size / samples
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = n = 0
            for sy in range(samples):
                for sx in range(samples):
                    c = colour((px * samples + sx + 0.5) * step, (py * samples + sy + 0.5) * step)
                    if c:
                        r += c[0]; g += c[1]; b += c[2]; n += 1
            if n:
                row += bytes((round(r / n), round(g / n), round(b / n), round(255 * n / samples ** 2)))
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return rows


def png(size):
    raw = b"".join(b"\x00" + row for row in render(size))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


open(sys.argv[1], "wb").write(png(256))

# The icon: one PNG entry per size (Windows Vista and later read PNG entries).
if len(sys.argv) > 2:
    sizes = [16, 20, 24, 32, 40, 48, 64, 256]
    images = [png(s) for s in sizes]
    offset = 6 + 16 * len(sizes)
    header = struct.pack("<HHH", 0, 1, len(sizes))
    entries = b""
    for s, data in zip(sizes, images):
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    open(sys.argv[2], "wb").write(header + entries + b"".join(images))
