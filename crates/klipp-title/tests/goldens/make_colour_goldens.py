"""Reference rasters of the colour test fonts (#3238).

    python make_colour_goldens.py

Needs freetype-py 2.5.1 (FreeType 2.13.2 inside) and skia-python
144.0.post2.

- COLR v0, drawn by FreeType (FT_LOAD_COLOR), which blends each layer in
  its palette colour over the ones below, the foreground colour black:
  for each glyph and size, `<font>-<char>-<px>.rgba`, a header of four
  little-endian i32 (width, height, left, top: the bitmap's left edge and
  top from the glyph's origin, y up) and then straight RGBA rows, top
  first.
- COLR v1, drawn by Skia (which Chrome draws COLR v1 with), the
  foreground colour black: for each of COLRV1_GLYPHS at 48 px,
  `colrv1/<glyph>.png`, straight RGBA, and its left, top (as above) and
  character in `colrv1/origins.json`.

Klipp's raster of each glyph is held to its reference.
"""

import json
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


COLRV1_FONT = "test_glyphs-glyf_colr_1.ttf"
COLRV1_PX = 48
COLRV1_GLYPHS = [
    "linear_repeat_0_1", "linear_gradient_extend_mode_pad", "linear_gradient_extend_mode_repeat",
    "linear_gradient_extend_mode_reflect", "radial_contained_gradient_extend_mode_pad",
    "radial_contained_gradient_extend_mode_repeat", "radial_contained_gradient_extend_mode_reflect",
    "radial_horizontal_gradient_extend_mode_pad", "radial_horizontal_gradient_extend_mode_repeat",
    "radial_horizontal_gradient_extend_mode_reflect", "sweep_0_360_pad_narrow", "sweep_60_300_reflect_wide",
    "sweep_-45_45_repeat_narrow", "sweep_270_440_pad_wide", "sweep_90_0_repeat_narrow",
    "sweep_coincident_stops_forward_blue_red_pad", "sweep_coincident_angles_forward_linen_gray_pad",
    "scale_0.5_1.5_center_500.0_500.0", "rotate_25_center_500.0_500.0", "skew_-10_20_center_500.0_500.0",
    "transform_matrix_0.9659_0.2588_-0.2588_0.9659_0_0", "translate_200_200",
    "composite_CLEAR", "composite_SRC", "composite_DEST", "composite_SRC_OVER", "composite_DEST_OVER",
    "composite_SRC_IN", "composite_DEST_IN", "composite_SRC_OUT", "composite_DEST_OUT", "composite_SRC_ATOP",
    "composite_DEST_ATOP", "composite_XOR", "composite_PLUS", "composite_SCREEN", "composite_OVERLAY",
    "composite_DARKEN", "composite_LIGHTEN", "composite_COLOR_DODGE", "composite_COLOR_BURN",
    "composite_HARD_LIGHT", "composite_SOFT_LIGHT", "composite_DIFFERENCE", "composite_EXCLUSION",
    "composite_MULTIPLY", "composite_HSL_HUE", "composite_HSL_SATURATION", "composite_HSL_COLOR",
    "composite_HSL_LUMINOSITY", "foreground_color_linear_alpha_0.3", "foreground_color_radial_alpha_1",
    "foreground_color_solid_alpha_1", "clip_box_center", "inset_clipped_radial_reflect",
    "gradient_p2_skewed", "colored_circles_v1", "solid_colorline_alpha",
    "paint_glyph_nested_rotate_center_translate", "no_cycle_multi_colrglyph",
]


def skia_goldens():
    import skia
    from fontTools.ttLib import TTFont

    path = os.path.join(FONTS, COLRV1_FONT)
    cmap = {name: code for code, name in TTFont(path).getBestCmap().items()}
    typeface = skia.Typeface.MakeFromFile(path)
    font = skia.Font(typeface, COLRV1_PX)
    font.setEdging(skia.Font.Edging.kAntiAlias)
    font.setHinting(skia.FontHinting.kNone)
    font.setSubpixel(True)
    out = os.path.join(HERE, "colrv1")
    os.makedirs(out, exist_ok=True)
    origins = {}
    for name in COLRV1_GLYPHS:
        text = chr(cmap[name])
        glyph = font.textToGlyphs(text)[0]
        bounds = font.getBounds([glyph])[0]
        left, top = int(bounds.left()) - 2, int(bounds.top()) - 2
        width, height = int(bounds.right()) + 2 - left, int(bounds.bottom()) + 2 - top
        surface = skia.Surface(skia.ImageInfo.MakeN32Premul(max(width, 1), max(height, 1)))
        canvas = surface.getCanvas()
        canvas.clear(skia.ColorTRANSPARENT)
        canvas.drawString(text, -left, -top, font, skia.Paint(Color=skia.ColorBLACK, AntiAlias=True))
        image = surface.makeImageSnapshot()
        image.save(os.path.join(out, f"{name}.png"), skia.kPNG)
        origins[name] = [left, -top, cmap[name]]
    with open(os.path.join(out, "origins.json"), "w") as f:
        json.dump({"font": COLRV1_FONT, "px": COLRV1_PX, "origins": origins}, f, indent=0, sort_keys=True)
        f.write("\n")


def main():
    skia_goldens()
    for font, char, px in CASES:
        stem = os.path.splitext(font)[0]
        with open(os.path.join(HERE, f"{stem}-{char}-{px}.rgba"), "wb") as f:
            f.write(render(font, char, px))


if __name__ == "__main__":
    main()
