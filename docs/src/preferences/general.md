# General Preferences

Access general preferences from **Edit → Preferences → General**.

## Auto-Save

Klipp automatically saves your project at regular intervals to prevent data loss.

- **Enable auto-save** — toggle automatic saving (default: on)
- **Interval** — time between auto-saves (default: 60 seconds while project is dirty)
- **Location** — auto-saves are stored alongside the project file

Auto-save runs in the background and does not interrupt editing.

## Backup

Project backup behaviour:

- **Keep backups** — number of backup copies to retain (default: 5)
- **Backup on open** — create a backup each time a project is opened (default: on)

Backups are stored in the project's `.klipp` directory with timestamps, allowing recovery of previous versions.

## Project Defaults

Default values for new projects:

- **Default frame rate** (default: 24 fps)
- **Default resolution** (default: 1920×1080)
- **Default audio sample rate** (default: 48 000 Hz)
- **Default still image duration** (default: 5 seconds)
- **Default working colour space** (default: ACEScg)
- **Default ODT** (default: sRGB)

Per-project overrides live in **Project → Settings**.

## Timeline Defaults

- **Default transition duration** (default: 30 frames)
- **Snapping enabled** (default: on)
- **Magnetic timeline** (default: on)

## Inspector Settings

- **Show all parameters** — display all effect parameters including advanced options
- **Scrubby slider sensitivity** — how fast values change when dragging (low / medium / high)
- **Parameter precision** — number of decimal places shown for numeric values

## Undo

- **Undo history depth** — maximum number of undo steps to retain (default: 100)

A deeper undo history uses more memory but allows reverting further back. Drag operations are coalesced into single undo steps.

## File Browser

- **Show hidden files** — toggle dotfile visibility in the import picker
- **Quick Look hover delay** — milliseconds before hover-preview activates (default: 300)
- **Recent files** — number of recently opened projects in the launch screen (default: 10)

## Recent Watched Folders

Folders you've imported from. Klipp can re-scan a watched folder to detect newly added media without you re-importing.
