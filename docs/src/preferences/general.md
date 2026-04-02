# General Preferences

Access general preferences from **Edit > Preferences > General**.

## Auto-Save

Klipp automatically saves your project at regular intervals to prevent data loss.

- **Enable auto-save** -- Toggle automatic saving on or off (default: on)
- **Interval** -- Time between auto-saves in minutes (default: 5 minutes)
- **Location** -- Auto-saves are stored alongside the project file

Auto-save creates a backup copy without interrupting your work. The save operation runs in the background.

## Backup

Configure project backup behavior:

- **Keep backups** -- Number of backup copies to retain (default: 5)
- **Backup on open** -- Create a backup each time a project is opened (default: on)

Backups are stored in the project's `.klipp` directory with timestamps, allowing you to recover previous versions if needed.

## Project Defaults

Set default values for new projects:

- **Default frame rate** -- Frame rate for new sequences (default: 24 fps)
- **Default resolution** -- Resolution for new sequences (default: 1920x1080)
- **Default audio sample rate** -- Sample rate for audio output (default: 48000 Hz)
- **Default still image duration** -- Duration assigned to imported still images (default: 5 seconds)

These defaults can be overridden per-project in the project settings.

## Timeline Defaults

- **Default transition duration** -- Length of transitions when applied (default: 30 frames)
- **Snapping enabled** -- Whether magnetic snapping is on by default (default: on)

## Inspector Settings

- **Show all parameters** -- Display all effect parameters including advanced options
- **Scrubby slider sensitivity** -- How fast values change when dragging (low/medium/high)
- **Parameter precision** -- Number of decimal places shown for numeric values

## Undo

- **Undo history depth** -- Maximum number of undo steps to retain (default: 100)

A deeper undo history uses more memory but allows you to revert further back in your editing session.
