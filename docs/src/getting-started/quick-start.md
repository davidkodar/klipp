# Quick Start

This guide walks you through creating your first project in Klipp, from launch to export, and introduces the core panels along the way.

## 1. Launch Klipp

```bash
klipp            # installed Arch/Omarchy package
# For developers in the source checkout: cargo r or cargo rr
```

Klipp opens with a launch screen. From there you can create a new project or open a recent one.

## 2. Create a New Project

Click **New Project** on the launch screen, or use **File → New Project** (**Ctrl+N**) once the editor is open. Choose a location and name. Klipp saves projects as `.klipp` directories containing all metadata, thumbnails, and per-clip cached data.

## 3. Import Media

Import your footage using one of these methods:

- **File → Import Media** (or **Ctrl+I**)
- Drag files from your file manager into the media browser
- Drag a folder onto the browser to import recursively

Klipp accepts most common video, audio, and image formats. Imported media appears in the media browser with thumbnails and (for video) a filmstrip strip across the bottom of each tile.

## 4. Browse Your Media

In the media browser:

- **Hover** over a clip thumbnail to *skim* through it. The viewer follows your cursor across the filmstrip.
- **Switch views** between grid and list using the icons at the top of the browser.
- **Bins** organise media into folders. Right-click in the bin tree for create/rename.
- **Sort** by name, date, duration, or size from the sort menu.
- **Search** with the box at the top — matches name, tags, and bin path.

## 5. Add Clips to the Timeline

Select a clip in the media browser, then:

- Press **,** to insert at the playhead (ripples the clips after it down)
- Press **.** to overwrite at the playhead
- Drag the clip directly onto a timeline track

Klipp's timeline is *magnetic* by default — gaps automatically close when you delete or move clips. See [Magnetic Timeline](../editing/magnetic-timeline.md) for the full set of behaviours.

## 6. Edit on the Timeline

Tool shortcuts:

| Key | Tool |
|---|---|
| **V** | Select — move and trim clips |
| **B** | Blade — split clips at the playhead |
| **R** | Range — select portions of clips |

Transport:

| Key | Action |
|---|---|
| **Space** | Play / pause |
| **J / K / L** | Shuttle reverse / pause / forward |
| **← / →** | Step one frame backward / forward |
| **Shift + ← / →** | Step ten frames |

Trim by dragging clip edges. Drop a clip on top of another and Klipp asks whether to insert (ripple) or overwrite.

## 7. Set In and Out Points

- **I** — mark in point at playhead
- **O** — mark out point at playhead
- **Shift+I** / **Shift+O** — clear in / out
- **Alt+X** — clear both

## 8. Adjust Clip Properties

Select a clip on the timeline. The **Inspector** on the right shows tabs:

- **Video** — Transform, opacity, GPU effects, source colourspace override
- **Audio** — Volume, pan, EQ, fade curves
- **Effects** — Per-clip effect chain with reorder + bypass
- **Info** — Source metadata (codec, resolution, frame rate, colourspace)

Most parameters can be keyframed by clicking the diamond icon next to the value. See [Keyframes](../color/keyframes.md).

## 9. Apply an AI Mask

Klipp ships with on-device AI tools for masking and object isolation:

1. Press **Alt+A** to open the AI Tools panel.
2. Pick a task: *Remove Background*, *Isolate Object*, or *Detect Objects*.
3. Klipp auto-selects the best model for that task. To override, expand **Advanced** and pick a different model.
4. Click **Run on current frame**. The first run downloads the model (~40–225 MB) — subsequent runs use the cache.
5. When complete, click **Apply to clip**. Klipp inserts an `ExternalMatte` node into the clip's colour graph and wires it to the first Corrector.

See [AI Tools Overview](../ai-tools/overview.md) for the full workflow.

## 10. Grade Colour

Open the **Color** workspace from **Window → Workspaces → Color**, or click the colour panel button. From here:

- **Wheels** — Lift, Gamma, Gain, Offset
- **Curves** — RGB and luma
- **HSL** — Hue/saturation/luminance qualifiers
- **Scopes** — Waveform, vectorscope, histogram, parade

Klipp's pipeline is linear-light throughout, working in **Rec.709 linear** unless you choose ACEScg or Rec.2020 in Project Info. Source clips with camera-log encoding (Sony SLog3, Canon Log, etc.) are auto-detected and decoded through their inverse log curve. See [Working Colorspaces](../color/working-colorspaces.md) for details.

## 11. Export

Go to **File → Export** (**Ctrl+E**). Choose a preset or configure:

- **Container** — MP4, MKV, MOV
- **Video codec** — H.264, H.265, ProRes, DNxHD
- **Resolution and frame rate**
- **Audio codec** — AAC, FLAC, MP3
- **Output Display Transform** — sRGB, Rec.709, Rec.2020, P3-D65

Click **Export**. Klipp renders in the background; you can keep editing.

## Tips

- The **Command Palette** (**Ctrl+K**) is the fastest way to find any command. Just start typing.
- **Performance overlay** (**P**) shows decode backend, GPU effects status, frame drops, and CPU/memory.
- **Auto-save** runs every 60 seconds while the project is dirty.
- Project files are plain JSON inside the `.klipp` directory — version-controllable.

## Next Steps

- [Working with Clips](../editing/clips.md) — trimming, slipping, sliding, replacing
- [Magnetic Timeline](../editing/magnetic-timeline.md) — the full set of FCP-parity behaviours
- [Color Grading](../color/grading.md) — wheels, curves, qualifiers, node graph
- [AI Tools](../ai-tools/overview.md) — masking, isolation, detection
- [Keyboard Shortcuts](../editing/shortcuts.md) — full reference
