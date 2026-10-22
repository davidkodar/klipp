# User Interface Overview

Klipp's interface follows a standard NLE layout with customizable panels and workspace presets.

## Default Layout

Three columns over a timeline, with one rule: every panel has one job and every tool has one home.

```
+--------------------------------------------------------------+
| Klipp  File Edit Clip ...     Edit | Colour | Export  [Q] [E]|
+-----------+--------------------------------------+-----------+
|           |      Source        |     Program     |           |
|  Browser  |      viewer        |     viewer      | Inspector |
|           |  transport         |  transport      |           |
|           +--------------------------------------+           |
|           |  tools | INS OVR | snap skim | M   zoom          |
|           |  Timeline                            |           |
+-----------+--------------------------------------+-----------+
| status bar                                                   |
+--------------------------------------------------------------+
```

- **App bar** — Brand, the menu bar, the workspace switch (Edit, Colour, Export) and the two global actions: Render Queue and Export. Nothing on it acts on the timeline.
- **Browser** — Left column: Files, Media and Library tabs.
- **Viewers** — Centre top. Source and program side by side, or one viewer (**View → Dual Viewers**). Each viewer owns its transport, its overlays and, on the program side, the scopes and viewer-layout buttons.
- **Timeline** — Centre bottom. Its own toolbar carries everything that changes how an edit behaves: the edit tools, insert / overwrite, ripple, snap, skimming, markers and zoom. A selects timeline can be stacked above it.
- **Inspector** — Right column. Video, Audio, Colour, Effects and Info tabs for the selection (Title or Generator for those clips), the sequence when nothing is selected.
- **AI Tools** — Optional panel (Alt+A) for AI-assisted masking, isolation and detection.

## Workspaces

The switch in the app bar chooses what the centre and right columns are for:

- **Edit** — Source and program viewers, the full timeline with its toolbar and sequence tabs, the clip inspector.
- **Colour** — One program viewer, a strip of graded thumbnails for moving between shots, the scope dock below, the grade in the right column.
- **Export** — One program viewer, the timeline for the in/out range, the export settings in the right column. Ctrl+E opens the same settings as a dialog from the other workspaces.

Each workspace remembers its own viewer layout (one or two viewers) and, for Colour, its own timeline height. **Window** lists the built-in layouts (including Audio Mixing) and the ones you save with **Save Workspace...**.

## Panel Visibility

Show or hide panels from the **Window** menu: Nodes, Inspector, Browser, Curve Editor, Audio Mixer, History, Markers, Edit List, Subtitle Editor, Transcript, Project Notes, Edit Plan, Colour Check, Delivery Check, Sound Over Time, Narrate, AI Tools and Plugins. The **Tools** menu opens the dialogs: Audio Plugins, AI Models, Cache Manager, Render Queue, Project Doctor and System Probe.

## Navigation

- Click any panel to give it focus.
- The **Command Palette** (**Ctrl+K**) searches every command by name.
- The status bar at the bottom of the window shows background jobs, the selection and the timeline's state.

## Themes

Klipp ships a theme registry with eight built-in themes (Dark, Cut, Light, Slate, Nord, Solarized Dark, Cream, High Contrast). User themes can be added by dropping a TOML file into `~/.config/klipp/themes/`.

Theme definitions cover:

- Surface colours (background, raised, overlay)
- Text and accent colours
- Border and divider hues
- Polarity-aware overlays — tooltips, popups, and toasts derive their backgrounds from the theme so light themes do not produce dark-on-dark or unreadable amber-on-amber combinations

UI scale and theme can be changed in **Edit → Preferences... → Appearance**, and **View → Interface Size** sets the scale directly. See [Appearance & Themes](../preferences/appearance.md) for the full reference.

## Adaptive Sizing

All panels adapt to the available space. The Inspector reflows controls when narrow (single-column layout); the Browser scales tile sizes with width; the AI Tools panel caps at 500 px to prevent runaway growth on long task tile labels.

Scrubby sliders — click-and-drag horizontally on any numeric value — work everywhere a number can be edited.
