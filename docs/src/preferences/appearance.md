# Appearance & Themes

Access appearance preferences from **Edit → Preferences... → Appearance**.

## Theme

**Follow system theme** follows the desktop's theme where Klipp can read it: on Omarchy, switching the theme there changes Klipp's live.

Otherwise pick one. Klipp uses a theme registry — themes are data, not hardcoded. The dropdown lists every theme Klipp has discovered:

- **Built-in themes** — bundled with Klipp, compiled in via `include_str!`: Dark (the neutral grading-suite look with a blue accent), Cut (warm greys, cobalt accent, white playhead, dialogue-green / music-teal / effects-violet roles), Light, Slate, Nord, Solarized Dark, Cream and High Contrast. Every theme owns its accent; there is no separate accent-colour preference.
- **User themes** — TOML files in `~/.config/klipp/themes/`. Drop a TOML into that directory and Klipp picks it up on next launch (or when you run `Reload Themes` from the command palette).

## Theme File Format

A theme TOML defines colours, polarity, and accent settings:

```toml
name = "My Theme"
polarity = "dark"      # "dark" or "light" — informs overlay logic

[colors]
bg_base    = "#0e0f10"
bg_raised  = "#181a1c"
text       = "#e6e8ea"
accent     = "#f5b041"
border     = "#2a2c30"
# … more keys; see existing themes for the full set
```

The `polarity` field drives Klipp's overlay logic — tooltip and toast backgrounds derive from theme tokens (`bg_raised`, `text`) rather than hardcoded dark hex, so a light theme produces light tooltips with dark text and vice versa.

## UI Scale

**UI scale** sets the size of the whole interface; **Auto** matches your system's DPI settings. **View → Interface Size** sets it from the menu. Changes take effect immediately.

## Role Colours

Clips are coloured on the timeline by their role: Video, Dialogue, Music, SFX, Title, B-Roll, Ambience, Foley, Voiceover and Graphics. **Edit...** beside a role changes its colour.
