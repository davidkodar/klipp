# Viewer

The viewer panel displays the current frame at the playhead position and provides monitoring tools for evaluating your video.

## Video Preview

The viewer renders the composited output of all visible tracks at the playhead position, on the GPU. During playback it updates at your monitor's refresh rate against the master clock, so sub-frame timing is honest even when the source frame rate doesn't match the display rate.

Klipp downscales high-resolution footage for playback to the maximum playback resolution set in **Preferences → Playback**, and the timeline's options can lower quality further or play from proxies. Exports always render at full resolution.

## One Viewer or Two

Source and program viewers sit side by side by default. The layout button under the program viewer, or **View → Dual Viewers**, collapses them to one viewer; a Source / Program switch next to the timecode then chooses which one it shows. Loading a clip from the browser switches the single viewer to Source. Each workspace remembers its own choice.

The source viewer's buttons insert (,), overwrite (.), make a subclip and match frames; see [Source Viewer](./source-viewer.md).

## Scopes

The scopes button under the program viewer (**Alt+V**) shows the scopes, overlaid on the viewer or in their own window, or in the scopes dock in the Colour workspace. See [Scopes](../color/scopes.md).

## Zoom

- **Fit** — Scale the video to fit the viewer area (default)
- **View → Viewer Zoom** — a fixed zoom, including 100%
- **Scroll wheel** — Zoom in and out on the viewer

## On-Picture Controls

With a clip selected, the viewer draws handles to move, scale and rotate it, and a title's handles to move and edit its text (double-click text to edit it). A clip's position keys show as a motion path you can drag. The Mask tool (P) draws and edits masks here, and a tracked surface shows its corners to place and track.

## Overlays

From the **View** menu or the viewer's options menu:

- **Safe Areas** (Alt+S) — title-safe and action-safe guides.
- **Grid** (Alt+G) — a rule-of-thirds grid.
- **Zebras** (Alt+Z) — stripes over overexposed areas.
- **False Colour** (Alt+F) — luminance mapped to colours for exposure; the palette is chosen and edited under **View → Palette**.
- **Focus Peaking** (Ctrl+P) — outlines in-focus edges.
- **Checkerboard** — shows transparent areas as a checkerboard.
- **Before / After** (Alt+B) — compare the graded picture with the ungraded one.

For HDR projects, the viewer can preview HDR, show HDR false colour, or send the picture to a reference output window.

A project's viewing LUT, set on the Colour Management page, is applied to the viewer only; the scopes can read before or after it.

## Performance Overlay

Press **Ctrl+Shift+P** to toggle the performance overlay, which shows:

- Frame rate against its target, and the frame budget
- Decode backend (NVDEC, VAAPI, or Software) and proxy use
- Dropped frames
- CPU usage and memory

**Ctrl+Alt+P** records a performance session to a file.

## Frame Grab

**View → Extract Frame to File…** (Ctrl+Alt+S) saves the viewer's frame as an image; **Extract Frame to Bin** (Ctrl+Alt+B) adds it to the media browser.

## Fullscreen

Press **F11** to toggle the fullscreen viewer. Press **F11** or **Escape** to return.
