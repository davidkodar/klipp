# Quick Start

This guide walks you through creating your first project in Klipp, from launch to export.

## 1. Launch Klipp

```bash
cargo rr
```

Klipp opens with an empty project. The interface shows the viewer at top-left, the media browser at top-right, the timeline at the bottom, and the inspector on the far right.

## 2. Create a New Project

Go to **File > New Project** (or press **Ctrl+N**). Choose a location and name for your project. Klipp saves projects as `.klipp` directories containing all project metadata.

## 3. Import Media

Import your footage using one of these methods:

- **File > Import Media** (or press **Ctrl+I**)
- Drag files from your file manager into the media browser

Klipp accepts most common video, audio, and image formats. Imported media appears in the media browser panel.

## 4. Browse Your Media

In the media browser, hover over a clip thumbnail to **skim** through it. Switch between grid and list views using the icons at the top of the browser. Use bins to organize your media into folders.

## 5. Add Clips to the Timeline

Select a clip in the media browser and use one of these methods:

- Press **W** to insert at the playhead position (ripples subsequent clips)
- Press **D** to overwrite at the playhead position
- Drag the clip directly onto a timeline track

## 6. Edit on the Timeline

- **V** -- Select tool for moving and trimming clips
- **B** -- Blade tool for splitting clips at the playhead
- **R** -- Range tool for selecting portions of clips
- **Left/Right arrows** -- Step one frame backward/forward
- **Space** -- Play/pause
- **J/K/L** -- Shuttle reverse/pause/forward

Trim clips by dragging their edges. The magnetic timeline automatically closes gaps when you ripple delete.

## 7. Set In and Out Points

- Press **I** to mark an in point at the playhead
- Press **O** to mark an out point at the playhead
- Press **Alt+I** or **Alt+O** to clear in/out points

## 8. Preview Your Edit

Press **Space** to play your timeline. Use **J/K/L** for variable-speed shuttling. The viewer shows your video with optional overlays (safe areas, grid, zebra stripes) accessible from the View menu.

## 9. Add Effects and Color

Select a clip on the timeline and open the inspector panel:

- **Video tab** -- Adjust transform, opacity, and apply GPU effects
- **Audio tab** -- Set volume, pan, and EQ
- **Color tab** -- Grade with color wheels, curves, and hue/saturation

## 10. Export

Go to **File > Export** (or press **Ctrl+E**) to open the export dialog. Choose a preset or configure:

- Container format (MP4, MKV, MOV)
- Video codec (H.264, H.265, ProRes, DNxHD)
- Resolution and frame rate
- Audio codec (AAC, FLAC, MP3)

Click **Export** to render your project.

## Next Steps

- Learn about [timeline editing](../editing/clips.md) in depth
- Explore [color grading](../color/grading.md) tools
- Set up [keyboard shortcuts](../editing/shortcuts.md) to match your workflow
