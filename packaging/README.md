# Packaging

An Arch / Omarchy `makepkg` recipe that builds Klipp from source and
installs it as a pacman package.

```bash
cd packaging
makepkg -si
```

`makepkg` reads [`PKGBUILD`](PKGBUILD), pulls the build tools, runs the
release build against the checkout this directory sits in (no second
clone), and installs the package. Uninstall with `pacman -R klipp-git`.

## What it installs

| Path | What |
|---|---|
| `/usr/lib/klipp/klipp` | the editor (the `klipp-app` binary) |
| `/usr/lib/klipp/klipp-hdr-out` | the HDR reference presenter the editor spawns |
| `/usr/lib/klipp/klipp-mcp` | the Model Context Protocol server |
| `/usr/lib/klipp/assets/luts/` | the bundled 3D-LUT library |
| `/usr/bin/klipp`, `/usr/bin/klipp-mcp` | symlinks onto PATH |
| `/usr/share/applications/klipp.desktop` | the launcher entry |
| `/usr/share/icons/hicolor/*/apps/klipp.png` | the app icon |

The binaries and `assets/luts` are kept in one directory on purpose:
Klipp finds the LUT library next to its executable, and the editor finds
`klipp-hdr-out` the same way. The `/usr/bin` entries are symlinks, which
Linux resolves through `/proc/self/exe`, so the lookup still lands in
`/usr/lib/klipp`.

## Out-of-the-box guarantees

- **No CUDA / NVIDIA required.** The default build runs the vision models
  on CPU and Whisper on Vulkan, and decode / encode fall back to
  software. `cudnn` is an *optional* accelerator (see `optdepends`).
- **Whisper works on any GPU** through Vulkan; its shaders are compiled
  into the binary at build time, so no runtime shader toolchain is
  needed.
- **The MCP tools are available immediately** — point an MCP client at
  the `klipp-mcp` command while Klipp is running.
- **The AI model weights are not bundled** (licensing): they download on
  first use, verified by SHA-256 against the manifest baked into the
  binary, into `~/.cache/klipp/models/`.
- **Vision-model runtime.** The ONNX models (BiRefNet, RF-DETR) load a
  system `libonnxruntime.so` at runtime, so the package **depends** on
  `onnxruntime` — they run on CPU out of the box. MobileSAM (pure-Rust
  Candle) and Whisper (Vulkan) need nothing extra. For NVIDIA GPU
  acceleration of the ONNX models, install a CUDA `onnxruntime` and point
  `ORT_DYLIB_PATH` at it; the app detects the GPU vendor and shows the
  runtime status in its AI panel. (Building from source without this
  package, you install `onnxruntime` yourself — see the top-level README.)

## Dependencies and credits

`depends` are the system packages Klipp links or runs against; `makepkg`
and pacman resolve them. They are the user's own system packages under
their own licences — Klipp does not redistribute them. `optdepends` add
optional acceleration (`cudnn`) or extra plugins for the frei0r and LV2
hosts (`frei0r-plugins`, `lsp-plugins-lv2`, `calf`). The third-party
assets Klipp *does* bundle — LUTs, fonts and icons — are credited in the
top-level [`README.md`](../README.md).
