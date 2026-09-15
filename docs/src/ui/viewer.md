# Viewer

The viewer panel displays the current frame at the playhead position and provides monitoring tools for evaluating your video.

## Video Preview

The viewer renders the composited output of all visible tracks at the playhead position. During playback, it updates at your monitor's refresh rate using wall-clock interpolation against the master clock — sub-frame timing is honest even when the source frame rate doesn't match the display rate.

Klipp automatically downscales high-resolution footage for playback to keep playback smooth. For example, 4K source material is scaled to 1080p during editing, while exports always render at full resolution.

## One Viewer or Two

Source and program viewers sit side by side by default. The layout button under the program viewer, or **View → Dual Viewers**, collapses them to one viewer that fills the row; a Source / Program switch next to the timecode then chooses which one it shows. Loading a clip from the browser switches the single viewer to Source.

## Scopes

The scopes button under the program viewer (**Alt+V**) opens the floating scope window, or the scopes dock in the Color workspace.

## Zoom Controls

- **Fit** — Scale the video to fit the viewer area (default)
- **100%** — View at actual pixel resolution
- **Scroll wheel** — Zoom in/out on the viewer
- Zoom level is displayed in the viewer corner

## Overlays

Access overlays from **View → Overlays** or the viewer's overlay menu:

### Safe Areas
Title-safe (80%) and action-safe (90%) boundary guides for broadcast-friendly composition.

### Grid
Rule-of-thirds grid for composition guidance.

### Zebra Stripes
Highlights overexposed areas with diagonal stripes. Configurable threshold (typically 70–100 IRE). Useful for monitoring exposure while grading.

### False Color
Maps luminance values to colours for exposure analysis:
- **Purple/Blue** — Underexposed
- **Green** — Mid-tones (correct exposure)
- **Yellow** — Highlights approaching clipping
- **Red** — Clipped / overexposed

## Performance Overlay

Press **P** to toggle the performance overlay, which displays:

- UI frames per second
- Decode backend (NVDEC, VAAPI, or Software)
- Playback state
- Frame queue depth
- Dropped frame count
- CPU usage percentage (process-specific, EMA-smoothed)
- Memory usage in MB

This overlay is invaluable for diagnosing playback issues and verifying that hardware acceleration is active.

## Frame Grab

Capture the current viewer frame as a still image. Exports a PNG at the current output resolution. Access from the viewer's context menu or the File menu.

## Fullscreen

Press **F11** to toggle fullscreen viewer mode. The viewer expands to the full window with all panels hidden — useful for client review or final QC. Press **F11** or **Escape** to return to the standard layout.
