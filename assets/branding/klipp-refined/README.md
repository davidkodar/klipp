# Klipp refined logo

Vector refinement of the amber filmstrip / editing-playhead concept. The app
embeds these icons in its project selection screen, toolbar, empty viewer, and
window icon. The desktop installation icon pack remains separate.

- `klipp-icon.svg`: full vector master, transparent outside the charcoal tile.
- `klipp-icon-small.svg`: simplified geometry for 16–32 px, without perforations,
  with a heavier playhead and wider frame borders.
- `klipp-logo.svg`: stacked lockup with a custom outlined geometric wordmark.
  All letters are paths; no font dependencies.
- `klipp-icon-{size}.png`: transparent PNG exports, 16 through 1024 px.
- `klipp-logo.png`: transparent 640 × 796 lockup.
- `preview.png` / `preview.svg`: palette and actual-size review sheet.

Palette: muted amber `#D99A38`, neutral charcoal `#242629`, soft white `#EAE7E0`.
Use the white wordmark on dark surfaces. All artwork uses solid fills, without
textures, shadows, gradients, embedded bitmaps, or external resources.

Regenerate with `python3 assets/branding/klipp-refined/build.py` from the repository
root. Requires Python 3 and `rsvg-convert`. The script uses only Python's standard
library. Vector geometry is authored directly, not traced from the image preview.

The PNG renders and 16–128 px review sheet were visually inspected. Tiny sizes
prioritize the frame/edit silhouette over photographic-film detail.
