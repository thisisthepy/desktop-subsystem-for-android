# Writes theme\logo.png and theme\logo.ico (run: uv run make-logo.py logo.png logo.ico).
# Draws the setup logo: two overlapping window frames, one per task, in the theme colour.
import struct, sys, zlib

W = H = 64
BG = (0, 0, 0, 0)
ACCENT = (61, 220, 132, 255)    # Android-like green, only a colour, no mark
DARK = (24, 40, 52, 255)

def inside(x, y, x0, y0, x1, y1):
    return x0 <= x < x1 and y0 <= y < y1

px = []
for y in range(H):
    row = []
    for x in range(W):
        c = BG
        # back window
        if inside(x, y, 6, 8, 44, 40):
            c = DARK if not inside(x, y, 9, 16, 41, 37) else (40, 64, 80, 255)
        # front window
        if inside(x, y, 20, 22, 58, 56):
            c = ACCENT if not inside(x, y, 23, 30, 55, 53) else (232, 250, 240, 255)
        row.append(c)
    px.append(row)

raw = b"".join(b"\x00" + b"".join(struct.pack("BBBB", *p) for p in row) for row in px)
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)

# The same picture as an icon: an ICO whose one entry is the PNG (Windows Vista and later).
if len(sys.argv) > 2:
    entry = struct.pack("<BBBBHHII", W, H, 0, 0, 1, 32, len(png), 6 + 16)
    open(sys.argv[2], "wb").write(struct.pack("<HHH", 0, 1, 1) + entry + png)
