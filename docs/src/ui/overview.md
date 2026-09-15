# User Interface Overview

Klipp's interface follows a standard NLE layout with customizable panels and workspace presets.

## Default Layout

The default workspace arranges four main areas:

```
+---------------------------+------------------+
|                           |                  |
|         Viewer            |  Media Browser   |
|                           |                  |
+---------------------------+------------------+
|                                    | Inspector|
|             Timeline               |          |
|                                    |          |
+------------------------------------+----------+
```

- **Menu Bar** — Across the top of the window with all menus and the toolbar.
- **Viewer** — Top-left. Displays the current frame at the playhead position.
- **Source Viewer** — Optional second viewer for browsing source media without affecting the timeline.
- **Media Browser** — Top-right. Imported media with grid/list views, bins, and skimming.
- **Timeline** — Bottom area. Multi-track timeline with filmstrip thumbnails and waveforms.
- **Inspector** — Right side. Context-sensitive panel showing video, audio, effects, and info tabs for the selected clip.
- **AI Tools** — Optional panel (Alt+A). Three-state machine for AI-assisted masking, isolation, and detection.

## Workspaces

Klipp provides workspace presets accessible from **Window → Workspaces**:

- **Default** — Balanced layout for general editing with all primary panels visible.
- **Color Grading** — Expanded viewer and color panel with scopes visible, timeline compressed.
- **Audio Mixing** — Audio tracks prominent with expanded meters, EQ, and volume controls.

## Panel Visibility

Toggle individual panels from the **Window** menu:

- Viewer
- Source Viewer
- Media Browser
- Timeline
- Inspector
- Color Panel
- Scopes
- Audio Meters
- AI Tools (Alt+A)
- AI Models dialog

## Navigation

- Click any panel to give it focus.
- The **Command Palette** (**Ctrl+Shift+P**) provides fuzzy-search access to every command.
- The status bar at the bottom of the timeline shows clip count, selected clip info, filmstrip statistics, and zoom level.

## Themes

Klipp ships a theme registry with eight built-in themes (Dark, Cut, Light, Slate, Nord, Solarized Dark, Cream, High Contrast). User themes can be added by dropping a TOML file into `~/.config/klipp/themes/`.

Theme definitions cover:

- Surface colours (background, raised, overlay)
- Text and accent colours
- Border and divider hues
- Polarity-aware overlays — tooltips, popups, and toasts derive their backgrounds from the theme so light themes do not produce dark-on-dark or unreadable amber-on-amber combinations

UI scale and theme can be changed in **Edit → Preferences → Appearance**. See [Appearance & Themes](../preferences/appearance.md) for the full reference.

## Adaptive Sizing

All panels adapt to the available space. The Inspector reflows controls when narrow (single-column layout); the Browser scales tile sizes with width; the AI Tools panel caps at 500 px to prevent runaway growth on long task tile labels.

Scrubby sliders — click-and-drag horizontally on any numeric value — work everywhere a number can be edited.
