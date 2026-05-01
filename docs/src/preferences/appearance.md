# Appearance & Themes

Access appearance preferences from **Edit → Preferences → Appearance**.

## Theme

Klipp uses a theme registry — themes are data, not hardcoded. The dropdown lists every theme Klipp has discovered:

- **Built-in themes** — bundled with Klipp, compiled in via `include_str!`. Examples: Klipp Dark, Klipp Light, Klipp Amber, Klipp Slate.
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

## Adding a Built-in Theme

For Klipp contributors: drop a `themes/*.toml` file in `crates/klipp-ui/assets/themes/` and add two lines to `theme/registry.rs` to register it. Built-ins survive across user installations; user themes are local.

## UI Scale

Adjust the overall interface scaling factor:

- **Auto** — match system DPI settings (default)
- **100%** — Standard size
- **125%** — Slightly larger; good for high-DPI displays
- **150%** — Large; suitable for 4K monitors at native resolution
- **175%** — Extra large
- **200%** — Double size

UI scale affects all panels, text, buttons, and controls uniformly. Changes take effect immediately.

## Accent Colour

Most themes ship with a configured accent colour. Override per session in this preferences pane:

- **Default** — use the theme's configured accent
- **Custom** — pick any colour with the colour picker

The accent is used for:
- Selected items in the browser and timeline
- Active tool indicators
- Playhead colour
- Button highlights and focus rings

## Role Colours

Timeline clips can be assigned role colours for visual organisation:

- **Video** — default colour for video clips
- **Audio** — default colour for audio clips
- **Titles** — colour for title / generator clips
- **Effects** — colour for adjustment layers and effect clips

Each role colour appears as a tint bar along the top edge of clips on the timeline.

## Font Size

Adjust the base font size independently of UI scale:

- **Small** — Compact text, fits more information
- **Medium** — Default size
- **Large** — Easier to read, uses more space

## Contrast Rules

Klipp follows two strict contrast rules in code:

1. **Amber / gold backgrounds always use dark text.** Never light text on amber — fails WCAG and looks washed out.
2. **Popup, toast, and tooltip backgrounds derive from theme polarity.** Hardcoded dark hex on a light theme produces dark-on-dark unreadable popups; theme-derived backgrounds always remain readable.

These rules mean adding a new theme rarely produces accessibility regressions, because the chrome adapts automatically.
