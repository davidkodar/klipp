"""Make KlippTestColrV0.ttf, a small COLR v0 test font (#3238).

    python make_colr_test_font.py

Needs fontTools (4.55). Klipp's own test data, under Klipp's licence; it
draws nothing from another font.

At 1000 units per em:

- A (U+0041): a colour glyph of three layers over palette 0: a red square
  filling the em box, a green square inside it, and a blue triangle at
  half opacity over both, one layer in the text's own colour on top (a
  small bar, palette index 0xFFFF).
- B (U+0042): the same red square's outline with no COLR record, an
  ordinary glyph.
"""

import os

from fontTools.colorLib.builder import buildCOLR, buildCPAL
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

HERE = os.path.dirname(os.path.abspath(__file__))


def polygon(points):
    pen = TTGlyphPen(None)
    pen.moveTo(points[0])
    for p in points[1:]:
        pen.lineTo(p)
    pen.closePath()
    return pen.glyph()


def main():
    square = [(0, 0), (0, 1000), (1000, 1000), (1000, 0)]
    glyphs = {
        ".notdef": polygon([(100, 0), (100, 700), (500, 700), (500, 0)]),
        "A": polygon(square),
        "B": polygon(square),
        "red": polygon(square),
        "green": polygon([(200, 200), (200, 800), (800, 800), (800, 200)]),
        "blue": polygon([(0, 0), (500, 1000), (1000, 0)]),
        "bar": polygon([(400, 450), (400, 550), (600, 550), (600, 450)]),
    }
    order = list(glyphs)
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({0x41: "A", 0x42: "B"})
    fb.setupGlyf(glyphs)
    # Each glyph's left side bearing is its own left edge, as TrueType
    # wants: a renderer may place the outline by it.
    glyf = fb.font["glyf"]
    for name in order:
        glyf[name].recalcBounds(glyf)
    fb.setupHorizontalMetrics({name: (1000, glyf[name].xMin) for name in order})
    fb.setupHorizontalHeader(ascent=1000, descent=0)
    fb.setupNameTable({"familyName": "Klipp Test COLR", "styleName": "Regular"})
    fb.setupOS2(sTypoAscender=1000, sTypoDescender=0, usWinAscent=1000, usWinDescent=0)
    fb.setupPost()
    font = fb.font
    font["CPAL"] = buildCPAL([[(1.0, 0.0, 0.0, 1.0), (0.0, 0.627, 0.0, 1.0), (0.0, 0.0, 1.0, 0.5)]])
    font["COLR"] = buildCOLR({"A": [("red", 0), ("green", 1), ("blue", 2), ("bar", 0xFFFF)]}, version=0)
    font.save(os.path.join(HERE, "KlippTestColrV0.ttf"))


if __name__ == "__main__":
    main()
