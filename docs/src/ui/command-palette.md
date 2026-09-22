# Command Palette

The command palette is a search overlay over every command Klipp exposes. Open it with **Ctrl+K** and start typing: the list narrows to the entries whose name, category or shortcut contains what you typed, in the order they are registered.

It is the fastest way to:

- Run a command whose menu location you can't remember
- Avoid mouse travel for keyboard-heavy edit sessions
- Discover commands you didn't know existed (the full list is browsable with arrow keys when the search box is empty)

## Coverage

The palette is wired to every user-facing `AppCommand` variant, every menu entry, and every keyboard shortcut. New commands automatically appear in the palette via the `palette_anchors_present` test, which pins high-value entries against regression.

Examples of palette entries:

- `New Project`, `Save Project`, `Export…`
- `Insert Edit`, `Overwrite Edit`, `Split at Playhead`
- `AI Tools: Remove Background`, `AI Tools: Isolate Object`, `AI Tools: Detect Objects`
- `AI Models: Open dialog`, `AI Models: Verify all`
- `Stabilize Selected Clip`, `Detect Scenes on Selected Clip`
- `Toggle Performance Overlay`, `Toggle Source Viewer`
- `Switch Workspace: Default`, `Switch Workspace: Color Grading`
- `Open Preferences`, `Reload Themes`

## Search Behaviour

- **Substring matches** — typing `bg` matches `Remove Background`
- **Initialism matches** — typing `rmbg` also matches `Remove Background`
- **Recency boost** — recently invoked commands rise to the top
- **Categorical labels** — commands are prefixed with their category (`AI Tools:`, `Timeline:`, etc.) so you can narrow by context

## Keyboard

| Key | Action |
|---|---|
| **Ctrl+K** | Open / close palette |
| **↑ / ↓** | Move selection |
| **Enter** | Run selected command |
| **Esc** | Close without running |

## Why It Matters

Klipp has roughly 200 user-facing commands. Discovering them through menus alone is impractical. The palette is intended as the canonical entry point for any command whose name you remember more easily than its location.
