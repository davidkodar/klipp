# Viewer

The viewer panel displays the current frame at the playhead position and provides monitoring tools for evaluating your video.

## Video Preview

The viewer renders the composited output of all visible tracks at the playhead position. During playback, it updates at your monitor's refresh rate with smooth frame interpolation.

Klipp automatically downscales high-resolution footage for playback. For example, 4K source material is scaled to 1080p during editing for smooth performance, while exports render at full resolution.

## Zoom Controls

- **Fit** -- Scale the video to fit the viewer area (default)
- **100%** -- View at actual pixel resolution
- **Scroll wheel** -- Zoom in/out on the viewer
- Zoom level is displayed in the viewer corner

## Overlays

Access overlays from **View > Overlays** or the viewer's overlay menu:

### Safe Areas
Displays title-safe and action-safe boundary guides. Title safe (80%) ensures text is visible on all displays. Action safe (90%) marks the area where important visual action should remain.

### Grid
Overlays a rule-of-thirds grid for composition guidance.

### Zebra Stripes
Highlights overexposed areas with diagonal stripes. Configurable threshold (typically 70-100 IRE). Useful for monitoring exposure while shooting or grading.

### False Color
Maps luminance values to colors for exposure analysis:
- **Purple/Blue** -- Underexposed
- **Green** -- Mid-tones (correct exposure)
- **Yellow** -- Highlights approaching clipping
- **Red** -- Clipped/overexposed

## Performance Overlay

Press **P** to toggle the performance overlay, which displays:

- UI frames per second
- Decode backend (NVDEC, VAAPI, or Software)
- Playback state
- Frame queue depth
- Dropped frame count
- CPU usage percentage
- Memory usage in MB

This overlay is invaluable for diagnosing playback issues and verifying that hardware acceleration is active.

## Frame Grab

Capture the current viewer frame as a still image. Exports a PNG at the current output resolution. Access from the viewer's context menu or the File menu.
