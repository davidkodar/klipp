# Klipp identity

*Klipp* is Swedish for "cut". The mark is a K whose leg has been cut off at
the waist and slipped to the right, like a clip moved on a timeline. The stem
and arm are soft white; the moved piece is amber.

The app embeds these files in its window icon, project selection screen,
toolbar, empty viewer and About screens, and the package installs the icon
PNGs into the hicolor theme. The website serves copies of `klipp-icon.svg` and
`klipp-wordmark.svg`, which `build.py` writes into `website/assets/`.

- `klipp-icon.svg`: the icon master, a charcoal tile with the cut K.
- `klipp-icon-small.svg`: 16–32 px version with a heavier stem and a wider
  cut, so the slip still shows at a few pixels.
- `klipp-icon-{size}.png`: PNG exports, 16 through 1024 px.
- `klipp-wordmark.svg` / `.png`: the "Klipp" wordmark in soft white, for dark
  backgrounds. `klipp-wordmark-light.*` is the charcoal version for light
  backgrounds. The K repeats the cut; the dot of the i is an amber square.
- `klipp-logo.svg` / `.png`: horizontal lockup, icon and wordmark, for dark
  backgrounds. `klipp-logo-light.*` is the version for light backgrounds.
- `preview.png` / `preview.svg`: review sheet with both lockups and the icon
  at the sizes it ships.

Palette: amber `#D99A38`, charcoal `#242629`, soft white `#EAE7E0`. Everything
is drawn from rectangles, straight cuts and arcs with solid fills: no fonts,
gradients, shadows or embedded bitmaps.

Regenerate with `python3 assets/branding/klipp-refined/build.py`. Requires
Python 3 and `rsvg-convert`; the script uses only the standard library.
