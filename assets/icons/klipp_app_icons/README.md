# Klipp icon — Linux pack

Full Linux icon set for the Klipp video editor, gold variant.

## Quick install

```bash
./install.sh
```

Installs to `~/.local/share/icons/hicolor/` and `~/.local/share/applications/`.
Only affects your user — no root required.

### System-wide install (optional)

```bash
sudo SYSTEM_INSTALL=1 ./install.sh
```

Installs to `/usr/share/icons/hicolor/` and `/usr/share/applications/` for all users.

## What's in this pack

```
klipp-icon-linux/
├── README.md                          ← you are here
├── install.sh                         ← one-command install
├── uninstall.sh                       ← clean removal
├── klipp.desktop                      ← freedesktop .desktop entry
├── hicolor/                           ← freedesktop hicolor theme
│   ├── 16x16/apps/klipp.png
│   ├── 22x22/apps/klipp.png
│   ├── 24x24/apps/klipp.png
│   ├── 32x32/apps/klipp.png
│   ├── 36x36/apps/klipp.png
│   ├── 48x48/apps/klipp.png
│   ├── 64x64/apps/klipp.png
│   ├── 72x72/apps/klipp.png
│   ├── 96x96/apps/klipp.png
│   ├── 128x128/apps/klipp.png
│   ├── 192x192/apps/klipp.png
│   ├── 256x256/apps/klipp.png
│   ├── 384x384/apps/klipp.png
│   ├── 512x512/apps/klipp.png
│   └── scalable/apps/klipp.svg        ← SVG (with embedded high-res PNG)
├── logo/
│   ├── klipp_logo_256.png             ← for inline documentation
│   ├── klipp_logo_512.png             ← for README headers
│   └── klipp_logo_1024.png            ← for marketing / press
└── web/                               ← bonus, useful if you host a Klipp site
    ├── favicon.ico                    ← multi-size favicon
    ├── favicon-16x16.png
    ├── favicon-32x32.png
    ├── favicon-48x48.png
    └── apple-touch-icon.png           ← 180×180, for iOS home-screen shortcuts
```

## After install

The icon will appear in:
- KDE menu / Plasma Launcher
- GNOME Activities
- Your taskbar/dock when Klipp runs
- File manager "Open With..." dialogs
- `krunner` / other application launchers

If the icon doesn't show up immediately, log out and back in, or restart your shell/panel. This forces the icon cache to refresh.

## Editing the .desktop file

By default `klipp.desktop` assumes the binary is in `$PATH` as `klipp`. If your
binary is installed elsewhere, edit the `Exec=` line in:

```
~/.local/share/applications/klipp.desktop
```

Example for a custom location:
```
Exec=/path/to/klipp/target/release/klipp-app %F
```

The `%F` passes any files dropped onto the icon or opened with "Open With Klipp".

## File format notes

- **All PNGs**: lossless, with alpha transparency (rounded corners cut out)
- **SVG**: contains an embedded high-resolution PNG, not true vector paths.
  It will scale reasonably but won't be pixel-perfect at arbitrary sizes like
  a real vector would. If you later produce a vector master, drop it in
  `hicolor/scalable/apps/klipp.svg` to replace this fallback.

## Uninstalling

```bash
./uninstall.sh
```

Removes all icon files and the `.desktop` entry. Does not remove the Klipp
binary itself.

## Regenerating

The source master is the 1024×1024 PNG used to produce all sizes here. The
bundled `build_linux_icons.py` script (kept in your original working copy)
can regenerate the pack from any 1024×1024 master PNG.
