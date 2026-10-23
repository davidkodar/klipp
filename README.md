<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/branding/klipp-refined/klipp-logo.svg">
    <img src="assets/branding/klipp-refined/klipp-logo-light.svg" alt="Klipp" height="72">
  </picture>
</h1>

**A native video editor for Linux.**

Edit on a magnetic timeline, grade in scene-linear colour, and automate workflows with local AI tools and an MCP-compatible assistant.

**Pre-release · Vulkan GPU required · Arch package or source · GPL-3.0-only**

Klipp is under active development. Expect rough edges and breaking changes; broad plugin compatibility, reliability hardening, and AMD/Intel GPU validation are ongoing. The first release candidate, [0.1.0-rc.1](https://github.com/davidkodar/klipp/releases/tag/v0.1.0-rc.1), is out as an unsigned Arch package; there are no stability guarantees yet.

[![Klipp editing workspace with titles, video clips and a music waveform](website/assets/editing.webp)](website/assets/editing.webp)

[Colour workspace](website/assets/colour.webp) · [Preview the demo](website/README.md#preview) · [Media credits](website/CREDITS.md)

*Klipp* is Swedish for “cut”.

## Create, finish, deliver

- **Edit:** magnetic multitrack editing, ripple and insert edits, synced multicam, a selects timeline, transitions, speed ramps and optical-flow slow motion, a curve editor for keyframes, and transcript-based cuts.
- **Picture:** one node graph per clip for effects, mattes, keys, transforms, LUTs and OFX/frei0r plug-ins, composited on the GPU.
- **Grade:** a scene-linear ACEScg pipeline, camera-log decoding, colour wheels and curves, RGB and 3D qualifiers, a grade library shared across projects, GPU scopes, and SDR/HDR delivery including HDR10+.
- **Titles:** OpenType text with complex scripts, vertical text, variable and colour fonts, animators and per-glyph motions, extruded 3D text, rolling credits, and a template library.
- **Mix:** multitrack audio with recorded track automation, EQ and dynamics, voice isolation, loudness and true-peak delivery, and dual-system sound sync.
- **Captions:** SRT, VTT, ASS, IMSC/TTML, SCC and EBU-STL in and out, captions embedded as CEA-608, and on-device translation.
- **Automate:** analyse footage, propose an edit plan, assemble a sequence, check it, and export through the control socket and an MCP server with over 120 tools.
- **Exchange:** import and export OTIO, FCPXML, EDL and AAF (through OpenTimelineIO), import FCP7 XML and MLT, export Kdenlive, and save your work as a `.klipp` project.

Explore the [feature reference](docs/project-reference.md#features) and [user guide](docs/src/SUMMARY.md).

## Try Klipp

The primary target is **x86_64 Arch Linux / Omarchy with a working Vulkan driver**. A Vulkan 1.1 GPU is required; integrated GPUs are supported in principle, with broader hardware validation ongoing. Other distributions are less tested.

To install the release candidate, download `klipp-0.1.0rc.1-1-x86_64.pkg.tar.zst` and `SHA256SUMS` from the [release page](https://github.com/davidkodar/klipp/releases/tag/v0.1.0-rc.1), then:

```bash
sha256sum -c --ignore-missing SHA256SUMS
sudo pacman -U klipp-0.1.0rc.1-1-x86_64.pkg.tar.zst
```

To build the latest code from source instead:

```bash
sudo pacman -Syu --needed base-devel git
git clone https://github.com/davidkodar/klipp.git
cd klipp/packaging
makepkg -si
klipp
```

Run `makepkg` as your normal user. If prompted for an ONNX Runtime provider, choose `onnxruntime-cpu` for the standard setup; NVIDIA/CUDA acceleration is optional. The first build needs substantial time and disk space. AI models download on demand.

See the [installation guide](packaging/README.md) for driver checks, updates, removal, and troubleshooting. For development and other distributions, see [Building](docs/project-reference.md#building).

## Local AI and external assistants

Klipp’s built-in transcription, vision, narration, and speech-enhancement models run on your machine. Models download when needed, and AI features can be disabled in Preferences.

The `klipp-mcp` server lets an external assistant work with a running Klipp instance. **That assistant may use a local or cloud model**, depending on your MCP client and its settings. Connecting an external assistant is separate from Klipp’s local inference.

Read about the [assistant workflow](docs/project-reference.md#the-ai-assistant), [models and licences](docs/src/ai-tools/models.md), and [optional online sound libraries](docs/project-reference.md#online-sound-libraries).

## Plugins

| Use | Formats |
| --- | --- |
| Video effects | OFX 1.5, frei0r filters |
| Audio effects | LADSPA, LV2, CLAP |

Compatibility depends on the plugin and the host features it requires. These hosts implement subsets of the formats; broad automated compatibility testing is in progress. VST2 and VST3 are not supported.

## Documentation and development

- [Quick start](docs/src/getting-started/quick-start.md)
- [User guide](docs/src/SUMMARY.md)
- [Build instructions](docs/project-reference.md#building)
- [Architecture](ARCHITECTURE.md)
- [Detailed features and credits](docs/project-reference.md)

The documentation is an mdBook: run `mdbook serve docs --open` to read it locally.

Issues and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Open an issue before non-trivial work and sign off commits under your own name. `scripts/install-hooks.sh` installs a pre-push hook that checks sign-offs and runs clippy and the affected crates' tests on your machine; GPU tests need a working GPU to mean anything.

Report security concerns through [SECURITY.md](SECURITY.md).

## Licence and credits

Klipp is licensed under [GPL-3.0-only](LICENSE). Bundled assets and downloaded models retain their respective licences; see [third-party credits](docs/project-reference.md#bundled-assets--third-party-credits) and [model licences](docs/project-reference.md#ai-models).
