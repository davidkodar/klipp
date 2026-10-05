# Klipp

**A native video editor for Linux.**

Edit on a magnetic timeline, grade in scene-linear colour, and automate workflows with local AI tools and an MCP-compatible assistant.

**Pre-release · Vulkan GPU required · Build from source · GPL-3.0-only**

Klipp is under active development. Expect rough edges and breaking changes; broad plugin compatibility, reliability hardening, and AMD/Intel GPU validation are ongoing. There are no pre-built releases or stability guarantees yet.

[![Klipp editing workspace with titles, video clips and a music waveform](website/assets/editing.webp)](website/assets/editing.webp)

[Colour workspace](website/assets/colour.webp) · [Preview the demo](website/README.md#preview) · [Media credits](website/CREDITS.md)

*Klipp* is Swedish for “cut”.

## Create, finish, deliver

- **Edit:** magnetic multitrack editing, ripple and insert edits, synced multicam, titles, transitions, subtitles, speed ramps, and transcript-based cuts.
- **Grade:** a scene-linear ACEScg pipeline, camera-log decoding, node-based grades, colour wheels and curves, LUTs, GPU scopes, and SDR/HDR delivery.
- **Mix:** multitrack audio, EQ and dynamics, voice isolation, loudness metering, and dual-system sound sync.
- **Automate:** analyse footage, propose an edit plan, assemble a sequence, check it, and export through the control socket and MCP server.
- **Exchange:** import OTIO, FCPXML, and MLT timelines, and save your work as a `.klipp` project.

Explore the [feature reference](docs/project-reference.md#features) and [user guide](docs/src/SUMMARY.md).

## Try Klipp

The primary target is **x86_64 Arch Linux / Omarchy with a working Vulkan driver**. A Vulkan 1.1 GPU is required; integrated GPUs are supported in principle, with broader hardware validation ongoing. Other distributions are less tested.

The repository is currently private: your GitHub account needs access, and Git must be authenticated before cloning.

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

Issues and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Open an issue before non-trivial work and sign off commits under your own name. Install the local pre-push gate with `scripts/install-hooks.sh`; it runs the checks, including the GPU tests, on your machine.

Report security concerns through [SECURITY.md](SECURITY.md).

## Licence and credits

Klipp is licensed under [GPL-3.0-only](LICENSE). Bundled assets and downloaded models retain their respective licences; see [third-party credits](docs/project-reference.md#bundled-assets--third-party-credits) and [model licences](docs/project-reference.md#ai-models).
