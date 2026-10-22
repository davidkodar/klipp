# Install on Arch / Omarchy

This is the supported source-install route for an up-to-date **x86_64 Arch or
Omarchy** system. It builds a local `klipp-git` pacman package. There is no
pre-built release to download yet. Other distributions are not validated by
this recipe.

## Downloadable releases

Release packaging and in-app update checks are described in [RELEASING.md](RELEASING.md).
A public download and signed package repository still need to be published.
Until then, use the source installation below. Installing a standalone package
does not automatically configure future system updates.

## Before starting

- A working Vulkan GPU driver is required for the editor. NVIDIA, AMD and Intel
  use their respective distribution drivers; installing `vulkan-icd-loader`
  alone does not install a GPU driver. Existing Omarchy installations normally
  already have one. `vulkaninfo --summary` (from `vulkan-tools`) can check it.
- Use your normal user account. `makepkg` must not run as root; it asks for sudo
  to install packages. Arch's [makepkg guide](https://wiki.archlinux.org/title/Makepkg)
  requires `base-devel` before building.
- Keep room for source, compiler output and model downloads: the checkout and
  its release build take about 5 GB, and the build dependencies about 3 GB more
  on a fresh system. A first build with 4 compiler jobs takes about 8 minutes on
  a fast desktop and longer on a laptop. Models are optional downloads and
  some are hundreds of megabytes each.

## Install

```bash
sudo pacman -Syu --needed base-devel git
git clone https://github.com/davidkodar/klipp.git
cd klipp/packaging
makepkg -si
```

Review the dependency prompts. The recipe installs the Rust/C/C++ build tools,
Vulkan headers and shader compiler, audio/video libraries, and ONNX Runtime.
If pacman asks for an `onnxruntime` provider, choose **onnxruntime-cpu** for the
standard setup; CUDA is not required. An existing compatible provider can stay.
If it asks for a `jack` provider, choose **pipewire-jack** on a PipeWire system
such as Omarchy, and `jack2` otherwise. If it asks for a `cargo` provider, `rust`
is the distribution compiler and `rustup` the toolchain manager; either works.
If you use rustup instead of the distribution Rust compiler, install its stable
toolchain first with `rustup toolchain install stable`.

The recipe runs `scripts/vulkan-sdk.sh` for you and builds the editor, HDR helper
and MCP server. You do not need to regenerate the logo artwork or install its
SVG rendering tool. The package uses this checkout's `target/` directory even
if your shell or Cargo configuration normally sends builds elsewhere.

Open **Klipp** from the app menu, or run:

```bash
klipp
```

## First-run check

1. Create a new project in a writable directory.
2. Import a short video with audio and put it on the timeline.
3. Play it, scrub, and check that both picture and sound work.
4. Save, close, and reopen the project.
5. Export a short section and play the exported file.

The [Quick Start](../docs/src/getting-started/quick-start.md) covers editing.
AI models download when requested and are verified against the bundled
manifest. Initial model downloads need network access; basic editing does not
require downloading models. The model manager reports runtime availability.

## Connect an AI assistant (optional)

No assistant is needed for normal editing. To let one work in Klipp with
you, open **Preferences → AI → Connect an assistant**. It shows:

- the installed `klipp-mcp` program (`/usr/bin/klipp-mcp`), which is
  what the assistant runs;
- **Check**, which asks the running Klipp for its status through that
  program and changes nothing;
- **Connect** and **Disconnect** for Claude Code and Codex when they are
  installed. These run the assistant's own `mcp add` and `mcp remove`
  commands, which change Klipp's entry and nothing else, and are safe to
  repeat;
- under **Another assistant**, settings to copy into any assistant that
  runs local MCP servers.

After connecting, start a new session in the assistant so it loads Klipp's
tools. Klipp must be running, with **AI assistant (MCP)** on under
Preferences → AI. Then ask it "Is Klipp connected?".

From a terminal instead:

```bash
klipp-mcp --check                                    # is Klipp reachable?
claude mcp add --scope user klipp -- /usr/bin/klipp-mcp
codex mcp add klipp -- /usr/bin/klipp-mcp
```

ChatGPT in the browser and other online assistants cannot run a program
on your computer, and Klipp does not offer a way in from the internet yet.

## Optional capabilities

- **ONNX AI on NVIDIA:** `onnxruntime-cuda` supplies the CUDA provider and depends
  on the required CUDA/cuDNN runtime packages. It replaces the CPU provider.
  Installing cuDNN alone does not turn a CPU-only ONNX Runtime into a CUDA one.
  Default builds keep Candle models on CPU; CUDA compilation for those is a
  separate developer option.
- **Narration:** requires the `piper` executable (Piper TTS) in addition to its
  downloaded voice. See [issue #113](https://github.com/davidkodar/klipp/issues/113)
  for the managed-runtime work still pending.
- **Screen recording:** requires `gpu-screen-recorder` or `wf-recorder`.
- **Extra effects:** frei0r and LV2 plugins are optional; see `optdepends` in
  [PKGBUILD](PKGBUILD).

## Update or remove

From the same checkout:

```bash
cd /path/to/klipp  # your original checkout
git pull --ff-only
cd packaging
makepkg -si
```

Close Klipp after saving your project and relaunch after installation to use
the new build. Since this is a VCS package, pacman shows a revision-count/hash
package version; About shows the application's declared release version and
build commit. These identify different things.

To uninstall:

```bash
sudo pacman -R klipp-git
```

Pacman does not remove your projects, user settings, or downloaded model cache.

## Troubleshooting

- **Missing headers, shader compiler or linker:** let `makepkg -s` resolve the
  declared dependencies. Do not skip dependency checks for a normal install.
- **Rustup has no stable compiler:** run `rustup toolchain install stable`.
- **Build is killed or memory runs out:** retry with fewer compiler jobs:
  `CARGO_BUILD_JOBS=2 makepkg -si`. Ensure compiler scratch and the checkout have
  sufficient disk space.
- **Window does not open / Vulkan errors:** run `klipp` from a terminal and
  check the GPU driver with `vulkaninfo --summary`.
- **Vision models report missing runtime:** check `pacman -Q onnxruntime-cpu`
  or `pacman -Q onnxruntime-cuda`. Remove an obsolete `ORT_DYLIB_PATH` override
  from the launch environment if it points at a missing library.

## Package contents

| Path | What |
|---|---|
| `/usr/lib/klipp/klipp` | Editor |
| `/usr/lib/klipp/klipp-hdr-out` | HDR reference presenter |
| `/usr/lib/klipp/klipp-mcp` | MCP server |
| `/usr/lib/klipp/assets/luts/` | Bundled LUT library and credits |
| `/usr/bin/klipp`, `/usr/bin/klipp-mcp` | Command symlinks |
| `/usr/share/applications/klipp.desktop` | Launcher entry |
| `/usr/share/icons/hicolor/*/apps/klipp.png` | Refined app icons |

Linux resolves the command symlinks to the actual executable, so the editor
finds the HDR helper and LUTs beside it. Fonts and other compiled-in assets
travel with the binaries. AI weights are downloaded into the user's cache,
not included in the package. Third-party asset credits are in the top-level
[README](../README.md).

## Packaging regression check

```bash
python3 -m unittest discover -s scripts/tests -p 'test_packaging.py' -v
```

This runs the actual `package()` function against a clean fixture checkout and
an empty staging directory. It checks the installed commands, helper, LUTs,
desktop entry and icon bytes, without installing anything on the host. It also
checks that the build selects the same output directory that packaging reads.
