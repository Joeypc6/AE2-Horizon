"""Composes AE2's tinted part textures at build time, as its colour handlers do at run time.

AE2 draws a terminal's screen as three grey light layers (bright, medium, dark), each tinted with a shade of
the cable colour (AEColor). Horizon has no tint for mod models, so the layers are tinted here and laid over the
display's front into one texture. Plain Python: 8-bit RGBA or RGB PNGs, non-interlaced.
"""

import struct
import zlib

# AEColor.TRANSPARENT (fluix): the dark, medium and bright shades.
FLUIX = {"dark": 0x1B2344, "medium": 0x895CA8, "bright": 0xD7BBEC}


def read_png(data):
    """(width, height, rows of RGBA bytes)."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, palette, trns = 8, b"", None, None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", chunk)
            assert depth == 8 and interlace == 0, "only 8-bit non-interlaced PNGs"
        elif kind == b"PLTE":
            palette = [chunk[i:i + 3] for i in range(0, len(chunk), 3)]
        elif kind == b"tRNS":
            trns = chunk
        elif kind == b"IDAT":
            idat += chunk
    channels = {6: 4, 2: 3, 3: 1, 0: 1, 4: 2}[color]
    raw = zlib.decompress(idat)
    stride = width * channels
    rows, prev, i = [], bytearray(stride), 0
    for _ in range(height):
        f, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - channels] if x >= channels else 0
            b, c = prev[x], prev[x - channels] if x >= channels else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        prev = line
        out = bytearray()
        for x in range(width):
            px = line[x * channels:(x + 1) * channels]
            if color == 6:
                out += px
            elif color == 2:
                out += px + b"\xff"
            elif color == 3:
                rgb = palette[px[0]]
                alpha = trns[px[0]] if trns and px[0] < len(trns) else 255
                out += rgb + bytes([alpha])
            elif color == 0:
                out += bytes([px[0]] * 3 + [255])
            else:
                out += bytes([px[0]] * 3 + [px[1]])
        rows.append(out)
    return width, height, rows


def write_png(width, height, rows):
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    chunk = lambda k, d: struct.pack(">I", len(d)) + k + d + struct.pack(">I", zlib.crc32(k + d) & 0xFFFFFFFF)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def compose(base, layers):
    """`base` with each (layer, rgb tint) drawn over it in order: the layer's grey times the tint, over its alpha."""
    w, h, out = read_png(base)
    out = [bytearray(r) for r in out]
    for data, tint in layers:
        lw, lh, rows = read_png(data)
        tr, tg, tb = tint >> 16 & 255, tint >> 8 & 255, tint & 255
        for y in range(min(h, lh)):
            for x in range(min(w, lw)):
                r, g, b, a = rows[y][x * 4:x * 4 + 4]
                if a == 0:
                    continue
                src = (r * tr // 255, g * tg // 255, b * tb // 255)
                dst = out[y][x * 4:x * 4 + 4]
                k = a / 255
                out[y][x * 4:x * 4 + 3] = bytes(round(s * k + d * (1 - k)) for s, d in zip(src, dst[:3]))
                out[y][x * 4 + 3] = max(dst[3], a)
    return write_png(w, h, out)
