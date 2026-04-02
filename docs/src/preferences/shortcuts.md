# Keyboard Shortcuts Preferences

Access keyboard shortcut preferences from **Edit > Preferences > Keyboard**.

## Shortcut Editor

The shortcut editor displays all available actions organized by category. Each row shows:

- **Action name** -- Description of what the shortcut does
- **Current binding** -- The currently assigned key combination
- **Default binding** -- The factory default (shown when modified)

## Remapping Shortcuts

To change a keyboard shortcut:

1. Open **Edit > Preferences > Keyboard**
2. Find the action you want to remap (use the search field to filter)
3. Click on the current binding for that action
4. Press the new key combination you want to assign
5. The binding updates immediately

To clear a binding, click the binding and press **Escape** or click the clear button.

## Conflict Detection

When you assign a key combination that is already used by another action, Klipp highlights the conflict:

- The conflicting actions are both highlighted in the list
- A warning message indicates which other action uses the same binding
- You can choose to proceed (the old binding is removed) or cancel

Klipp prevents assigning system-reserved combinations that would conflict with window management.

## Search

Type in the search field at the top of the shortcuts panel to filter actions by name. This helps you find specific shortcuts quickly in the full list.

## Categories

Shortcuts are organized into the following categories:

- **File** -- Project operations (new, open, save, export)
- **Edit** -- Clipboard and selection operations
- **Tools** -- Editing tool selection
- **Timeline** -- Timeline navigation and editing
- **Mark** -- In/out points and markers
- **Transport** -- Playback control
- **View** -- UI and overlay toggles

## Reset to Defaults

Click **Reset to Defaults** at the bottom of the shortcuts panel to restore all keyboard shortcuts to their factory default bindings. This cannot be undone, so confirm when prompted.

## Shortcut Persistence

Custom shortcut bindings are saved in your Klipp preferences and persist across sessions. They are stored separately from project files, so your shortcuts are the same regardless of which project is open.
