"""Make the colour-glyph test fonts from Noto Color Emoji (#3238).

    python make_colour_test_fonts.py /usr/share/fonts/noto/NotoColorEmoji.ttf

Needs fontTools (4.55). Writes, beside this script:

- NotoColorEmoji-cbdt-subset.ttf: the rainbow and the grinning face from
  Noto Color Emoji, its CBDT/CBLC strike as it is.
- NotoColorEmoji-sbix-subset.ttf: the same two glyphs with the same PNGs
  in an sbix table instead, the CBDT and CBLC tables removed. Each
  bitmap's origin offset is its CBDT bearing, so both fonts draw a glyph
  in the same place.
- NotoColorEmoji-sbix-cbdt-subset.ttf: the sbix font with the CBDT and
  CBLC tables put back, a font with two kinds of colour glyph.

Noto Color Emoji is under the SIL Open Font License 1.1 with no Reserved
Font Names (OFL-NotoColorEmoji.txt), which allows these modified copies
to be redistributed under the same licence.
"""

import os
import sys

from fontTools import subset
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables import sbixGlyph, sbixStrike

HERE = os.path.dirname(os.path.abspath(__file__))
TEXT_UNICODES = [0x1F308, 0x1F600]


def cbdt_subset(source, out):
    options = subset.Options()
    options.notdef_outline = True
    options.name_IDs = ["*"]
    font = TTFont(source)
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=TEXT_UNICODES)
    subsetter.subset(font)
    font.save(out)


def sbix_from(cbdt_path, out):
    font = TTFont(cbdt_path)
    cblc = font["CBLC"]
    cbdt = font["CBDT"]
    table = newTable("sbix")
    table.version = 1
    table.flags = 1
    for strike_record, strike_data in zip(cblc.strikes, cbdt.strikeData):
        ppem = strike_record.bitmapSizeTable.ppemY
        strike = sbixStrike.Strike(ppem=ppem, resolution=72)
        for name, glyph in strike_data.items():
            glyph.decompile()
            metrics = glyph.metrics
            strike.glyphs[name] = sbixGlyph.Glyph(
                glyphName=name,
                graphicType="png ",
                imageData=glyph.imageData,
                originOffsetX=metrics.BearingX,
                originOffsetY=metrics.BearingY - metrics.height,
            )
        table.strikes[ppem] = strike
    del font["CBDT"]
    del font["CBLC"]
    font["sbix"] = table
    font.save(out)


def main():
    source = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/noto/NotoColorEmoji.ttf"
    cbdt = os.path.join(HERE, "NotoColorEmoji-cbdt-subset.ttf")
    cbdt_subset(source, cbdt)
    sbix = os.path.join(HERE, "NotoColorEmoji-sbix-subset.ttf")
    sbix_from(cbdt, sbix)
    both = TTFont(sbix)
    bitmaps = TTFont(cbdt)
    both["CBDT"] = bitmaps["CBDT"]
    both["CBLC"] = bitmaps["CBLC"]
    both.save(os.path.join(HERE, "NotoColorEmoji-sbix-cbdt-subset.ttf"))


if __name__ == "__main__":
    main()
