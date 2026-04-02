# Timeline

The timeline is the primary editing surface where you arrange, trim, and organize your clips.

## Tracks

The timeline supports multiple video and audio tracks:

- **Video tracks** stack from bottom to top. Higher tracks composite over lower tracks.
- **Audio tracks** mix together. Each track has independent volume and pan.
- Add tracks from **Timeline > Add Video Track** or **Timeline > Add Audio Track**.
- Tracks display a header on the left with name, lock, mute/solo, and visibility toggles.

## Clips

Clips on the timeline represent segments of imported media. They display:

- **Filmstrip thumbnails** on video clips, showing frames at regular intervals
- **Audio waveforms** on audio clips and linked audio
- **Color tint bar** at the top edge for visual identification
- Clip name overlay

## Playhead

The playhead is a vertical line indicating the current position. Click anywhere on the ruler or track area to move the playhead. During playback, the playhead moves smoothly using wall-clock interpolation for sub-frame accuracy.

## Magnetic Timeline

Klipp uses a magnetic timeline model inspired by Final Cut Pro:

- Clips automatically snap together, eliminating gaps
- Moving a clip ripples adjacent clips to maintain continuity
- Deleting a clip closes the gap automatically (ripple delete)
- Override magnetic behavior by holding a modifier key while dragging

## Snapping

Toggle snapping with **N**. When enabled, clips snap to:

- The playhead
- Other clip boundaries
- In/out points
- Markers

A visual indicator appears when a snap occurs.

## Filmstrip Thumbnails

Video clips display filmstrip thumbnails that update as you zoom:

- Thumbnails are generated incrementally (~60 frames per clip)
- Progressive refinement: zooming in generates higher-density thumbnails
- Only viewport-visible tiles are rendered for performance
- Dark separators mark distinct frame boundaries

## Waveforms

Audio tracks and linked audio display waveforms showing amplitude over time. Waveforms update with zoom level and are rendered efficiently using cached data.

## Zoom and Navigation

- **Ctrl+=** / **Ctrl+-** -- Zoom in/out
- **Shift+Z** -- Zoom to fit the entire timeline
- **Scroll wheel** -- Horizontal scroll (or zoom with Ctrl held)
- **Home** / **End** -- Jump to timeline start/end

## Status Bar

An 18-pixel status bar below the timeline tracks shows:

- **Left side**: Clip count, selected clip name, and duration
- **Right side**: Filmstrip visible/total count and zoom level (pixels per second)
