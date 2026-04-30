# Keyboard Shortcuts Preferences

Access keyboard shortcut preferences from **Edit → Preferences → Keyboard**.

## Shortcut Editor

The shortcut editor displays all available actions organised by category. Each row shows:

- **Action name** — description of what the shortcut does
- **Current binding** — the currently assigned key combination
- **Default binding** — the factory default (shown when modified)

## Remapping Shortcuts

To change a keyboard shortcut:

1. Open **Edit → Preferences → Keyboard**
2. Find the action (use the search field to filter)
3. Click on the current binding
4. Press the new key combination
5. Binding updates immediately

To clear a binding, click the binding and press **Escape** or click the clear button.

## Conflict Detection

When you assign a key combination already used by another action, Klipp highlights the conflict:

- The conflicting actions are both highlighted in the list
- A warning indicates which other action uses the same binding
- You can proceed (the old binding is removed) or cancel

System-reserved combinations that conflict with window management are not assignable.

## Search

Type in the search field at the top of the shortcuts panel to filter actions by name. Helps locate shortcuts in the full list.

## Categories

Shortcuts are organised into these categories:

- **File** — Project operations (new, open, save, export)
- **Edit** — Clipboard and selection
- **Tools** — Editing tool selection
- **Timeline** — Timeline navigation and editing
- **Mark** — In/out points and markers
- **Transport** — Playback control
- **Clips** — Per-clip operations (enable, solo, transitions)
- **AI Tools** — Open panel, run task, apply mask
- **View** — UI and overlay toggles
- **Window** — Workspace switching, panel toggles

## Reset to Defaults

**Reset to Defaults** at the bottom of the shortcuts panel restores all keyboard shortcuts to their factory defaults. This cannot be undone, so confirm when prompted.

## Persistence

Custom shortcut bindings are saved at `~/.config/klipp/keybindings.json` and persist across sessions and Klipp updates. They are stored separately from project files.

## Default Reference

A complete list of default shortcuts is in the [Keyboard Shortcuts](../editing/shortcuts.md) reference.
