# Klipp Icon Pack — Spectrum Wheel

Dark variant (black bg) + Light variant (off-white bg).
Seamless 5-colour ring, continuous with no gaps, subtle inner ring, white/dark play triangle.

## svg/
- klipp-dark.svg / klipp-light.svg          — rounded corners (standard app icon shape)
- klipp-dark-square.svg / klipp-light-square.svg  — no rounding (for custom masking)

## png/dark/ and png/light/
Sizes: 16 22 24 32 48 64 96 128 144 180 192 256 512 1024 px

## Platform containers
| File                | Platform | Contains              |
|---------------------|----------|-----------------------|
| klipp-dark.ico      | Windows  | 16, 32, 48, 256 px    |
| klipp-light.ico     | Windows  | 16, 32, 48, 256 px    |
| klipp-dark.icns     | macOS    | 16–1024 + @2x Retina  |
| klipp-light.icns    | macOS    | 16–1024 + @2x Retina  |

## linux-hicolor/
XDG hicolor theme structure. Install:
  cp -r linux-hicolor/* ~/.local/share/icons/hicolor/
  gtk-update-icon-cache ~/.local/share/icons/hicolor/

## Quick reference
| Use case               | File                           |
|------------------------|--------------------------------|
| App window icon        | png/dark/klipp-dark-256.png    |
| KDE/GNOME launcher     | linux-hicolor/ tree            |
| Web favicon            | klipp-dark.ico                 |
| Apple touch icon       | png/dark/klipp-dark-180.png    |
| PWA manifest           | 192px + 512px dark PNGs        |
| Website / docs         | png/light/klipp-light-*.png    |
| macOS app bundle       | klipp-dark.icns                |
| Windows installer      | klipp-dark.ico                 |
