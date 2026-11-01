"""Reference rasters of the colour test fonts, drawn by FreeType (#3238).

    python make_colour_goldens.py

Needs freetype-py 2.5.1 (FreeType 2.13.2 inside). FreeType draws COLR v0
layers itself (FT_LOAD_COLOR), blending each layer in its palette colour
over the ones below, the foreground colour black. Writes beside this
script, for each glyph and size, `<font>-<char>-<px>.rgba`: a header of
four little-endian i32 (width, height, left, top: the bitmap's left edge
and top from the glyph's origin, y up) and then straight RGBA rows, top
first. Klipp's raster of the same glyph is held to it.
"""

import os
import struct

import freetype

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "..", "fonts")

CASES = [("KlippTestColrV0.ttf", "A", px) for px in (24, 64, 150)]


def render(font, char, px):
    face = freetype.Face(os.path.join(FONTS, font))
    face.set_pixel_sizes(0, px)
    face.load_char(char, freetype.FT_LOAD_COLOR | freetype.FT_LOAD_RENDER | freetype.FT_LOAD_NO_HINTING)
    slot = face.glyph
    bitmap = slot.bitmap
    if bitmap.pixel_mode != freetype.FT_PIXEL_MODE_BGRA:
        raise SystemExit(f"{font} {char}: FreeType drew no colour bitmap")
    width, height, pitch = bitmap.width, bitmap.rows, bitmap.pitch
    data = bytes(bitmap.buffer)
    rgba = bytearray()
    for y in range(height):
        row = data[y * pitch : y * pitch + width * 4]
        for x in range(width):
            b, g, r, a = row[x * 4 : x * 4 + 4]
            # FreeType's BGRA is premultiplied; the reference is straight.
            if a:
                r, g, b = (min(255, (c * 255 + a // 2) // a) for c in (r, g, b))
            rgba += bytes((r, g, b, a))
    return struct.pack("<4i", width, height, slot.bitmap_left, slot.bitmap_top) + bytes(rgba)


def main():
    for font, char, px in CASES:
        stem = os.path.splitext(font)[0]
        with open(os.path.join(HERE, f"{stem}-{char}-{px}.rgba"), "wb") as f:
            f.write(render(font, char, px))


if __name__ == "__main__":
    main()
