# Quick Start

This guide walks you through creating your first project in Klipp, from launch to export, and introduces the core panels along the way.

## 1. Launch Klipp

```bash
klipp            # installed package (Arch-based distributions)
# For developers in the source checkout: cargo r or cargo rr
```

On Ubuntu, Fedora and other distributions there is no package yet: build from source as [Installation](installation.md) describes.

Klipp opens with a welcome screen: your recent and pinned projects, everything in your projects folder, and a row of templates to start from.

## 2. Create a New Project

Click **New Project** on the welcome screen, or use **File → New Project...** (**Ctrl+N**) once the editor is open. Give it a name and a location, and choose a resolution and frame rate — or **Match first clip** to take them from the first clip you add. A template can also add starter content.

Klipp saves a project as a `.klipp` directory holding the project file and its per-project data.

## 3. Import Media

Import your footage using one of these methods:

- **File → Import Media...** (**Ctrl+I**)
- Drag files from your file manager into the media browser
- Drag a folder onto the browser to import recursively

Imported media appears in the media browser with thumbnails, and an import can be undone.

## 4. Browse Your Media

In the media browser:

- **Hover** over a clip thumbnail to *skim* through it; the viewer follows your cursor.
- **Switch views** between grid and list using the icons at the top of the browser.
- **Bins** organise media into folders; **smart bins** fill themselves from rules.
- **Search** with the box at the top (**Ctrl+F**) — by name, tags and metadata.

## 5. Add Clips to the Timeline

Select a clip in the media browser, then:

- Press **,** (or **F10**) to insert at the playhead, rippling the clips after it
- Press **.** to overwrite at the playhead
- Drag the clip directly onto a timeline track

Klipp's timeline is *magnetic* by default — gaps close when you delete or move clips. See [Magnetic Timeline](../editing/magnetic-timeline.md).

## 6. Edit on the Timeline

Tool shortcuts:

| Key | Tool |
|---|---|
| **V** | Select — move and trim clips |
| **T** | Trim |
| **B** | Blade — split clips where you click |
| **R** | Range — select portions of clips |
| **Y** | Text — draw a title in the viewer |
| **P** | Mask — draw a mask in the viewer |

**Ctrl+B** splits the selected clips at the playhead.

Transport:

| Key | Action |
|---|---|
| **Space** | Play / pause |
| **J / K / L** | Shuttle reverse / pause / forward |
| **← / →** | Step one frame backward / forward |
| **Shift + ← / →** | Step one second (or the number of frames set in Preferences) |

Trim by dragging clip edges.

## 7. Set In and Out Points

- **I** — mark in point at playhead
- **O** — mark out point at playhead
- **Shift+I** / **Shift+O** — clear in / out
- **Alt+X** — clear both
- **X** — set in and out to the clip under the playhead

## 8. Adjust Clip Properties

Select a clip on the timeline. The **Inspector** shows tabs for it:

- **Video** — transform, crop, opacity, source colourspace override
- **Audio** — volume, pan, EQ, fades
- **Colour** — the clip's grade
- **Effects** — the clip's effect stack: add, reorder, mix, mask and bypass
- **Info** — source metadata (codec, resolution, frame rate, colourspace)

A title shows a **Title** tab and a generator a **Generator** tab in place of Audio and Colour. With nothing selected, the Inspector shows the sequence.

Most parameters can be keyframed with the diamond next to the value; **◆ Keyframes** at the top of the Inspector shows each parameter's keys beside it. See [Keyframes](../color/keyframes.md).

## 9. Apply an AI Mask

Klipp ships with on-device AI tools for masking and object isolation:

1. Press **Alt+A** to open the AI Tools panel.
2. Pick a task: *Remove Background*, *Isolate Object*, or *Detect Objects*.
3. Klipp picks a model for that task. To override, expand **Advanced** and pick a different one.
4. Click **Run on current frame**. The first run downloads the model; later runs use the cache.
5. When complete, click **Apply to clip**. The matte is added to the clip's colour graph.

See [AI Tools Overview](../ai-tools/overview.md) for the full workflow.

## 10. Grade Colour

Switch to the **Colour** workspace with the switcher at the top of the window, or press **C** to show the colour panel. From here:

- **Wheels** — Lift, Gamma, Gain, Offset, log wheels and HDR Zones
- **Curves** — RGB, luma and hue curves
- **Qualifiers** — HSL, RGB and 3D keys
- **Nodes** — the clip's grade as a node graph
- **Scopes** (**Alt+V**) — waveform, vectorscope, histogram, parade

Klipp's pipeline is linear-light throughout, working in **Rec.709 linear** unless you choose ACEScg or Rec.2020 for the project. Source clips with camera-log encoding (Sony S-Log3, Canon Log, etc.) are auto-detected and decoded through their inverse log curve. See [Working Colorspaces](../color/working-colorspaces.md).

## 11. Export

Go to **File → Export...** (**Ctrl+E**). Choose a delivery preset or set:

- **Container** — MP4, MKV, MOV
- **Video codec** — H.264, H.265, AV1, VP9, ProRes, DNxHD, GIF
- **Resolution, frame rate and bit depth**
- **Audio** — codec, and a loudness target
- **Output Display Transform** — sRGB, Rec.709, Rec.2020, P3-D65

Click **Export**. Klipp renders in the background; you can keep editing. **Window → Delivery Check** looks over picture and sound before you deliver.

## Tips

- The **Command Palette** (**Ctrl+K**) is the fastest way to find any command. Just start typing.
- The **performance overlay** (**Ctrl+Shift+P**) shows decode backend, GPU status, frame drops, and CPU/memory.
- **Auto-save** runs every five minutes while the project has unsaved changes (Preferences → General).
- Project files are plain JSON inside the `.klipp` directory.

## Next Steps

- [Working with Clips](../editing/clips.md) — trimming, slipping, sliding, replacing
- [Magnetic Timeline](../editing/magnetic-timeline.md) — the full set of timeline behaviours
- [Color Grading](../color/grading.md) — wheels, curves, qualifiers, node graph
- [AI Tools](../ai-tools/overview.md) — masking, isolation, detection
- [Keyboard Shortcuts](../editing/shortcuts.md) — full reference
