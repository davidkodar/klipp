# Klipp project reference

Detailed features, developer build instructions, model information and third-party credits. For a quick introduction and installation, see [the README](../README.md).

## Features


### Editing

Magnetic multi-track timeline; insert and ripple edits, splits and ripple deletes; J and L cuts; markers with notes, colours and kinds, every change one undo; per-clip retime and reframe with speed ramps. Editing by transcript: select words in the transcript and delete them, and the picture and sound go with them; or sweep out the long pauses in one step. Clips can be moved by motion rather than by hand: `zoom`, `pan`, `follow` (the frame tracks a subject), `highlight` and `ramp` are first-class operations on the timeline, not keyframes you draw.

A **selects timeline** stacks above the main one for pulling takes, and a cut can be built from it in one step. **Match Clip Below** finds the source under a clip; **New Version at Aspect** makes a vertical or square version of a sequence, reframed on the subject; **Frame Glide** and optical-flow slow motion smooth retimed clips. Keyframes have a **Curve Editor** (several curves at once, a speed view, handle kinds, easing presets and snapping), a keyframe column in the Inspector, and a motion path drawn in the viewer.

### Picture graph

Each clip has one picture graph, shown in the Nodes dock: effects, grades, mattes, keys, 3D LUTs and colour-space transforms, transforms, merges, extra media sources, and OFX or frei0r plug-ins are all nodes in it. Any node can be viewed in the viewer, nodes can carry colour labels and reroutes, and a graph you build can be saved and reused as an effect. Mattes can mask any effect, and matte sequences can drive them over time. Projects are written at format revision 4.

### Multicam

Synced multicam groups, lined up by timecode, by their sound, or by timecode then sound, with each angle on a track of its own. Cut to an angle with the keys 1–9, the angle bar or the menu; the Source viewer becomes a grid of every angle, with the live one marked; and each cut is one undo step. The group decides whose sound is heard: one angle throughout, the sound following the picture, or none when a separate recorder carries it. Playback and export follow the same rule.

### Colour

- **Grading** — scene-linear node-graph grading (primary + HSL secondary qualifiers, curves, wheels, warper), 3D-LUT looks, BT.1886 and HDR grade encodings.
- **White balance** — Auto WB balances on the frame's typical neutral and refuses a reference it can't trust; the eyedropper beside it balances on the grey you click.
- **Timeline and shared grades** — a grade over the whole timeline alongside each clip's own, and stills and looks that carry a grade between clips without applying the source clip's log decode a second time.
- **Viewer LUT** — a monitor LUT applied to the picture on screen only; scopes, measurements and exports keep the graded frame.
- **Scopes** — GPU waveform, parade, vectorscope and histogram at native size, plus **false colour** whose skin band comes from the look the piece is being held to, not a fixed IRE window — so it agrees with the grading loop instead of offering a second opinion.
- **Measurement** — a **Colour Check** window reads the frame in the viewer and names the fault and the knob to turn: exposure metered off the face, casts on the neutrals, noise floor, banding, gamut, and whether the grade's nodes are in an order that can settle. A trend strip beneath it fills as you watch the piece, so drift across a sequence shows up on one axis.
- **Matching** — shots grouped into scenes by how they were lit, a hero picked per scene, the rest graded onto it, and the remaining distance reported. Matching compares the light, not the people.
- **Looks from a reference** — point at a picture and say "like this": the look's numbers are taken off a reference frame or the timeline's best-graded shot and become the target everything else is held to. The named presets are the fallback, not the normal path.
- **Qualifiers and keys** — HSL, RGB and 3D qualifiers, a luma key and a key mixer, with mattes that can mask any node.
- **Grade library** — grades saved across projects in a library and a shared library, grade flags, graded thumbnails, a wipe against another clip, track grades, and grade keyframes on the Colour page.
- **Faces** — a face detector, the cheek band read in CIELAB, and a skin secondary held inside a window tracked on the face, so a red jumper in shot doesn't turn with the skin.

### Audio

Multi-track mixer with one pan law for playback and export, and each clip's own volume, gain, pan and channel setting applied in the mix; audio crossfades; playback at 48 kHz with sinc resampling to whatever the output device runs at, on the device chosen in Preferences. Per-clip DSP: noise reduction, **Voice Isolation** (DeepFilterNet3 speech enhancement, on the machine), mains-hum removal, de-esser, gate, compressor, EQ, limiter. Volume keyframes; EBU R128 loudness metering and normalisation; dual-system sound sync by cross-correlation; clips normalised to a loudness or a peak in one step; track volume and pan automation that can be recorded live; music fitted to a length; audio roles (dialogue, music, effects, ambience) proposed by an on-device sound classifier; and delivery that holds true peak. A **Sound Over Time** window shows a whole file as frequency against time — where a hum starts, when a radio mic began to hiss, which two minutes have the air conditioning in them — with the findings named.

### Titles, transitions, effects

A title engine with OpenType shaping for complex scripts and right-to-left text, vertical text and ruby, variable fonts, font fallback and colour emoji; styled runs, named type styles, gradients and glow; paragraphs, tabs, lists, text on a path and fit-to-box; animators and keyed type, colour and size; entrances and staggered per-glyph motions; extruded 3D text; editing in the viewer with the Text tool; a built-in title library and templates; rolling credits; find and replace across titles; and missing-font replacement. Titles are composited on the GPU.

73 transitions, including piece transitions and a Smooth Cut for jump cuts, and 66 built-in GPU effects, among them particles, flicker, binocular vignette, temporal noise reduction, stabilisation and rolling-shutter correction.

### Screen recordings and demos

Screencasts defeat camera-tuned analysis — a four-minute demo reads as one frozen shot. Klipp analyses them on their own terms: a **beat sheet** built from stillness (where the screen settles, and the reveal that is the best moment to cut to), on-screen text read by OCR, and a `redact` operation that hides a patch of picture for a stretch. `auto_redact` covers every sensitive string the OCR found — emails, keys, passwords, card numbers — wherever the timeline shows it. The timeline find bar and the media pool search read that screen text too, so you can search a recording for what was on screen.

### The paper edit

An agent proposes an `EditPlan` — segments, joins, music, titles — and the **Edit Plan window** in the app is where the person whose word it waits on reads it and hits Approve, Build or Discard. Build lowers the plan to ops and applies them as one undo step, along exactly the path the socket takes, so what an agent would build is what the button builds.

### Sound design and music

`auto_sfx` places a whoosh on every cut, a click on every reveal the beat sheet found and a riser into the last cut, as one undo step. Beat detection lets a plan's segments snap to the music's bars. Royalty-free music and sound effects can be searched and previewed from inside Klipp (Jamendo and Freesound — see [Online sound libraries](#online-sound-libraries)).

### Media management

Bins and rule-based smart bins, markers, shot logging, metadata (EXIF stills, Sony/Canon camera XML sidecars), and CSV/ALE sheets. **Find by Picture** searches the pool by describing a shot in words. Project templates and a welcome screen start new work; a relink flow follows imports.

### Interchange and captions

Timelines: these formats carry the edit (clips, cuts and timing, and markers where the format has them), not grades, most effects or most titles. Import and export OTIO, FCPXML, CMX3600 EDL and AAF (AAF through OpenTimelineIO's converter, installed only with your consent); import FCP7 XML and MLT; export Kdenlive (cuts only). Each import ends with a report of what came across and what didn't, and another editor's project can be opened in one command.

Captions: SRT, VTT, ASS/SSA, TTML/DFXP/iTT (written as IMSC 1.1), SCC (CEA-608) and EBU-STL, read and written; captions can also be embedded in the video as CEA-608. Caption tracks carry a language and a visibility, and can be translated on the machine.

### On-device AI

Subject masking and segmentation (BiRefNet, MobileSAM, RF-DETR-Seg), speech transcription (Whisper Large v3 Turbo on the GPU via Vulkan), face detection (YuNet), on-screen text (PP-OCR) and a scratch narration voice (Piper) and speech enhancement for Voice Isolation (DeepFilterNet3), voice activity detection (Silero VAD), caption translation (OPUS-MT), audio roles (PANNs) and picture search (CLIP) — all local, all downloaded on demand and verified against a bundled manifest.

### Delivery

Export with CPU/GPU render parity and real HDR output; hardware encoding (NVENC on NVIDIA, AMF and VAAPI on AMD, VAAPI on Intel) that falls back to the software encoder when no hardware session is free; web presets that normalise to −14 LUFS, where those platforms play, while masters keep their level and Broadcast its R128 −23; an export with no path set goes to your Videos folder, named after the project; burnt-in or sidecar captions; a delivery check that covers picture **and** sound against the project's brief; HDR10+ metadata on HEVC PQ, Rec.2020 SDR, advice when an 8-bit delivery will band; audio-only exports as MP3, WAV or M4A; and review comments that come back as markers.

### Plugins

Video effects: OFX 1.5 and frei0r filters. Audio effects: LADSPA, LV2 and CLAP. Hosts implement a subset of each format; compatibility varies by plugin. Broad automated compatibility testing is still in progress. VST2 and VST3 are not supported.

## Building


### Prerequisites

```bash
# Omarchy / Arch / CachyOS (the primary target)
sudo pacman -S --needed base-devel git rust ffmpeg clang lld cmake pkgconf \
    vulkan-icd-loader vulkan-headers shaderc lilv onnxruntime-cpu espeak-ng

# Ubuntu / Debian
sudo apt install rustup ffmpeg libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libswscale-dev libswresample-dev libavdevice-dev \
    libclang-dev lld cmake pkg-config libvulkan-dev glslc libasound2-dev \
    liblilv-dev espeak-ng
```

Omarchy is Arch-based, so the `pacman` line covers it directly. On Omarchy or Arch you can skip the manual step entirely and use the [`packaging/`](../packaging/) recipe, which installs the dependencies.

The Vulkan headers and `glslc` are build-time only: the speech model runs on the GPU through Vulkan (NVIDIA, AMD or Intel) and its shaders are compiled into the binary. At runtime Klipp needs the GPU driver, the ffmpeg libraries, `libonnxruntime.so` (loaded when a vision model runs), `lilv` for LV2 plug-ins, and `espeak-ng` for the Kokoro voices. The Ubuntu / Debian line is less tested than the Arch one.

Two assistant tools shell out to a program if you want them: `narrate` needs `piper-tts`, and `record` needs `gpu-screen-recorder` or `wf-recorder`. Both are optional and everything else works without them.

For NVIDIA GPU acceleration of the ONNX / Candle AI models (recommended, not required — these fall back to CPU):

```bash
# Arch / CachyOS
sudo pacman -S onnxruntime-cuda  # replaces the CPU provider; pulls CUDA/cuDNN

# Ubuntu / Debian
sudo apt install libcudnn9-cuda-12
```

### Run

```bash
git clone https://github.com/davidkodar/klipp
cd klipp
./scripts/vulkan-sdk.sh   # once: lays out the Vulkan SDK directory the speech model's build expects
cargo r                   # debug build (aliased: run -p klipp-app)
cargo rr                  # release build
```

AI features ship enabled by default, including the CUDA execution provider for the ONNX models (BiRefNet, RF-DETR-Seg, OCR, faces), which is used when a CUDA `onnxruntime` is installed and falls back to the CPU otherwise. For the Candle models on an NVIDIA GPU as well:

```bash
cargo r-cuda        # adds Candle CUDA (MobileSAM, PANNs, CLIP, OPUS-MT); requires the CUDA Toolkit
```

For minimal builds without AI dependencies:

```bash
cargo run -p klipp-app --no-default-features
```

See [docs/src/getting-started/installation.md](../docs/src/getting-started/installation.md) for the build-time vs runtime CUDA distinction and full instructions.

## The AI assistant

Klipp exposes a running instance to an agent so an entire edit can be built through tools alone. Two crates carry it:

- **`klipp-control`** opens a Unix-socket JSON protocol on a running app: read the state, run analysers, apply undoable edit operations, grab frames and scopes, export.
- **`klipp-mcp`** wraps that socket as a [Model Context Protocol](https://modelcontextprotocol.io) server, so any MCP client can call the tools — over 120 of them, covering status and jobs, transport and screenshots, projects and sequences, media and analysis, finding shots, planning and applying edits (64 edit operations), the picture graph, colour, audio and music, titles and fonts, captions, multicam, markers, review and checks, export, and recording and narration. The package installs it as the `klipp-mcp` command; point your MCP client at it while Klipp is running, and the tools appear.

Through the control interface, an agent names clips and media by id and places by anchor, and Klipp resolves the frames. The loop it works in:

1. **Orient** — where it is in one call: the build, the project, the brief, the footage, the sequence, and what to do next.
2. **The brief** — what the piece is for: its length, platform, loudness, whether it is captioned, the cast and their names, and the named type styles its titles use. The check holds the piece to it and the export takes its size and loudness from it.
3. **Read the footage** — shot boundaries, loudness with the spectral bands that reveal wind, rumble, hum and hiss, silences, motion, transcript, who is in shot and how large, on-screen text, tempo and beat grid, and a beat sheet for screen recordings. `style_profile` reads a reference video's whole editing style — its cut cadence, whether it cuts on the beat, its look, its motion — so you can build in that feel from your own footage and your own music, never the reference's track.
4. **Find the cut** — clean cut points ranked (pauses first, then sentence ends and shot changes, then phrase breaks), a long take split into its keeps and drops, shots found by framing, people, objects, motion, speech or length.
5. **Plan** — a paper edit a person approves in the Edit Plan window before anything is built.
6. **Build** — place clips, joins, titles, music; motion; sound design; captions from the transcript. Each step is one undo.
7. **Finish** — check the render path against a chart first, adopt a look off a reference, match scenes to their hero, pull skin onto the band, clean wind / hum / hiss, sync dual-system sound.
8. **Look at it** — `review` renders a draft of the whole timeline, tiles it into a contact sheet, and runs the check: the agent sees the rhythm, the titles and the cards, rather than assuming.
9. **Deliver** — a quality check over picture and sound, then export.

The craft guidance the assistant follows lives in [`assets/craft/`](../assets/craft/) — the guide, the recipes and the reference are read by the model, not by the code, so they can be argued with and improved without a rebuild.

It can also make its own footage: `record` captures the screen (the focused window, a monitor or a region) and imports it, and `narrate` speaks a scratch voice track to cut to and replace later, at a speed and with a pause between sentences it chooses. A person does the same in **Window → Narrate**: type the words, pick a voice by name (each has a bundled sample to hear first), set the speed and pauses, preview it, and place it at the playhead on the Voice track. The ten natural-sounding Kokoro voices are one download of 87 MB; Heart is the default.

The socket also covers the things an agent would otherwise have to ask a person to do: create a project or a sequence, change the brief one field at a time, shape a clip's volume by hand (`volume`) and duck the music under what is heard over it (`duck`: a clip marked as music, effects or ambience doesn't count, nor does one set well under the music), build a multicam group and cut its angles, tag and relink media, take a screenshot of the whole window, and use markers as a note channel both ways. The agent marks what it is unsure of and why, and treats the editor's todo markers as work asked for.

### Turning it off

All of the on-device AI is **easy to turn off, one piece at a time**: **Preferences → AI** has a separate switch for speech transcription, for subject detection, for on-screen text, and for the AI assistant (the MCP server), so you can, say, keep transcription and turn the assistant off entirely. It also shows the "ready / off" status of each, a CPU-vs-GPU choice for transcription, and the models with their sizes and a link to the full model manager. A media item's right-click menu in the media pool has **Transcribe**, and the transcript panel has a Transcribe button, to run speech-to-text on demand even with automatic transcription off.

### Online sound libraries

When you request a music or sound-effect search, Klipp queries [Jamendo](https://www.jamendo.com/) and [Freesound](https://freesound.org/) for Creative-Commons audio. They need your own API keys, which live in `~/.config/klipp/keys.toml` (or the `KLIPP_JAMENDO_CLIENT_ID` / `KLIPP_FREESOUND_TOKEN` environment variables) and never in the source tree. Downloading Freesound originals also needs a sign-in under **Preferences → Accounts**; the token is kept in `~/.config/klipp/freesound-oauth.json`. Without keys the feature is simply absent. Built-in AI inference runs locally; model weights download on demand. External MCP clients may use cloud models and follow their own data-handling settings.

Update checks are **off by default**. Choosing **Check for updates** in About or Preferences queries GitHub for published Klipp releases. You can explicitly enable daily background checks in **Preferences → General → Updates** and turn them off there again. Checks send no project information; GitHub receives the request and network address. With automatic checks disabled, launching Klipp or simply opening Preferences makes no update request.

## Architecture

Klipp is a 42-crate Cargo workspace organised into layers, with dependencies flowing one way and no cycles. Every crate carries a `//!` overview; `ARCHITECTURE.md` is the system map.

| Crate | Responsibility |
|---|---|
| `klipp-core` | Foundation types: IDs, rational time, events, config, errors |
| `klipp-project` | `.klipp` project format and templates; OTIO, FCPXML, FCP7 XML, EDL, AAF and MLT import; the exports |
| `klipp-graph-format` | How a clip's picture graph is written in a project |
| `klipp-graph-lift` | Picture revisions and graph operations |
| `klipp-convert` | One-time project-format conversion (never linked into the app) |
| `klipp-captions` | Subtitle and caption formats |
| `klipp-timeline` | Sequence / track / clip model, the magnetic-timeline editor, the brief and the edit plan |
| `klipp-media` | Media pool, bins, thumbnails, waveforms, on-disk caches |
| `klipp-audio` | DSP effects, LUFS metering, multi-track mixer |
| `klipp-deepfilter` | DeepFilterNet3 inference for Voice Isolation, vendored from DeepFilterNet's libDF without `unsafe` |
| `klipp-audio-search` | Royalty-free music and SFX search (Jamendo, Freesound) |
| `klipp-color` | Colour grading, LUTs, scopes, camera-log decoders, CIELAB measurement, false colour, the chart self-check |
| `klipp-grade-model` | The primary grade's maths (lift/gamma/gain/offset, contrast, BT.1886) |
| `klipp-nodes` | The per-clip picture graph: model, node registry and executor |
| `klipp-engine` | Real-time playback: decode, composite, mix, output |
| `klipp-compose` | The one definition of how a timeline frame is rendered |
| `klipp-effects` | Built-in GPU effects and the shared scope pipeline (wgpu / Vulkan) |
| `klipp-transitions` | Transition pixel-math |
| `klipp-title` | Title / text engine: shaping, layout, glyphs, style runs, motions |
| `klipp-linebreak` | Unicode line breaking, vendored |
| `klipp-plugin` | OFX 1.5 plugin host (C ABI, dlopen) |
| `klipp-frei0r` | frei0r 1.x plugin host |
| `klipp-render` / `klipp-export` | Offline render pipeline and the export-queue model |
| `klipp-hdr-out` | HDR reference output shared by viewer and export |
| `klipp-validation` | Project validation and pre-flight checks |
| `klipp-ml` / `klipp-ml-ort` / `klipp-ml-candle` | Model manifest + ONNX (`ort`) and Candle inference backends |
| `klipp-analysis` | Footage analysers: shots, audio, motion, transcript, framing, beats, beat sheet, spectrogram, screen text |
| `klipp-document` | The edit history, the ops, and the lowering of an edit plan into them |
| `klipp-control` | Control surface for a running instance (socket protocol) |
| `klipp-mcp` | Model Context Protocol server |
| `klipp-jobs` | Background job scheduling primitives |
| `klipp-load-lock` | Serialises library loading and GPU start-up |
| `klipp-readiness` | Inventory of what runs on the GPU and what is ready |
| `klipp-ui` | The egui / eframe desktop editor surface |
| `klipp-app` | Binary entry point |
| `klipp-perf` / `klipp-smoke` / `klipp-testing` / `klipp-corpus` | Perf harness, headless engine harness, test scaffolding, reproducible pixel-check inputs |

## Tech Stack

- **UI** — [egui](https://github.com/emilk/egui)
- **GPU** — [wgpu](https://wgpu.rs/) (Vulkan backend)
- **Decode / Encode** — [ffmpeg-next](https://crates.io/crates/ffmpeg-next)
- **Audio** — [cpal](https://crates.io/crates/cpal) (ALSA, which PipeWire also serves)
- **AI inference** — [ort](https://crates.io/crates/ort) for BiRefNet, RF-DETR, OCR, faces and voice activity; [candle](https://crates.io/crates/candle-core) for MobileSAM, PANNs, CLIP and OPUS-MT; [tract](https://github.com/sonos/tract) for DeepFilterNet; [whisper-rs](https://crates.io/crates/whisper-rs) (Vulkan) for speech; [Piper](https://github.com/rhasspy/piper) for the scratch voice
- **Colour** — an in-house scene-linear ACEScg pipeline with camera-log auto-detection; the ACES output transforms are checked against OCIO, and the render path against a published ColorChecker

## AI Models


Models are downloaded on demand, verified by SHA-256 against the bundled manifest, and stored in `~/.local/share/klipp/models/`. Klipp does not redistribute weights — the manifest points at the upstream sources (HuggingFace, and GitHub for YuNet and DeepFilterNet3).

| Model | Role | Size | Licence |
|---|---|---|---|
| Whisper Large v3 Turbo (q5) | Speech transcription | ~574 MB | MIT (whisper.cpp / ggml) |
| Whisper Base | Small and quick, less accurate — the fallback on a slow machine | ~148 MB | MIT (whisper.cpp / ggml) |
| BiRefNet Lite | Subject masking | 224 MB | MIT |
| MobileSAM | Promptable segmentation | 40 MB | Apache-2.0 |
| RF-DETR-Seg (Preview / Medium) | Detection + segmentation, framing analysis | 134 / 139 MB | Apache-2.0 |
| YuNet | Face detection for the colour measurements | 233 KB | MIT |
| PP-OCRv3 det + PP-OCRv4 rec | On-screen text in screen recordings | ~2.4 MB + ~7.7 MB | Apache-2.0 |
| Piper voices (lighter, under "More voices"): LJSpeech (the fallback where Kokoro cannot run), Kristin, Norman, John, LibriTTS (US); Cori (UK); NST (Swedish) | Scratch narration voices | 63–137 MB each | MIT (Piper); voices public domain or CC0, LibriTTS CC BY 4.0 |
| Kokoro-82M (8-bit) + 10 voices, one download: Heart (the default), Bella, Nicole, Michael, Fenrir, Puck (US); Emma, Isabella, George, Fable (UK) | Natural-sounding narration (needs `espeak-ng`) | 86 MB + 0.5 MB a voice | Apache-2.0 |
| Piper en_US Lessac (medium) | Scratch narration voice, kept for older projects | 63 MB | **Research only, not for commercial use** (Blizzard 2013 Lessac licence) |
| DeepFilterNet3 | Voice Isolation (speech enhancement) | ~8 MB | MIT OR Apache-2.0 |
| Silero VAD v5 | Voice activity detection | 885 KB | MIT |
| OPUS-MT (14 language pairs: English to and from German, French, Spanish, Swedish, Italian, Dutch; English to Finnish and Danish) | Caption translation | ~300 MB + 5 MB vocabulary, a pair | CC-BY-4.0 |
| PANNs Cnn10 | Audio roles | 25 MB | CC-BY-4.0 |
| CLIP ViT-B/32 | Find by Picture | 605 MB | MIT |

See [docs/src/ai-tools/models.md](../docs/src/ai-tools/models.md) for the full picture, including licence audit notes.

## Bundled Assets & Third-Party Credits

### Update checking

- **ureq** — HTTPS client, licensed under MIT or Apache-2.0.
- **semver** — semantic release-version comparisons, licensed under MIT or Apache-2.0.

### LUTs (Colour Look-Up Tables)

Klipp ships ~300 film-emulation and cinematic LUTs for colour grading:

- **Film Emulation LUTs** — derived from [G'MIC](https://gmic.eu/) film simulation presets, packaged by [YahiaAngelo/Film-Luts](https://github.com/YahiaAngelo/Film-Luts). Includes Kodak Portra, Tri-X, Fuji Velvia, Superia, Provia, Agfa, Polaroid, and many more. Licensed under **MIT**.
- **Cinematic Blockbuster LUTs** — by [IRCGraphic](https://github.com/IRCGraphic/D-Cinelike-and-Normal-Blockbuster-LUTs). Licensed under **CC0 1.0** (public domain).

### Fonts

Klipp bundles these typefaces for its title tool, each under the **SIL Open Font License 1.1**:

- **Inter** — [Rasmus Andersson](https://rsms.me/inter/)
- **Bebas Neue** — [Ryoichi Tsunekawa / Dharma Type](https://www.dafont.com/bebas-neue.font)
- **Libre Baskerville** — [Impallari Type](https://github.com/impallari/Libre-Baskerville)
- **JetBrains Mono** — [JetBrains](https://www.jetbrains.com/lp/mono/)

### Icons

- **Phosphor Icons** — [phosphoricons.com](https://phosphoricons.com/). Licensed under **MIT**.

### Narration samples

`assets/narration-samples/` holds a few seconds of each Kokoro voice, so a voice can be heard before it is downloaded. They were spoken by [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache-2.0) through Klipp's own narration path.

### Colour reference data

The ColorChecker patch values the render-path self-check uses are the **BabelColor averages of the X-Rite ColorChecker Classic** — published figures, quoted rather than re-derived, which is the whole point of an external reference.

### Models

See the AI Models table above; each model retains its upstream attribution and licence.

### The effects browser's preview picture

The frame the effect cards render on, until the viewer has one of your
own, is [Bachalpsee, Lake, Mountains](https://pixabay.com/photos/bachalpsee-7572681/)
by **Himmelstraeume**, under the [Pixabay Content
License](https://pixabay.com/service/license-summary/) — free for
commercial use, no attribution required. Credited here anyway, so
nobody has to work out where it came from. Note for packagers: the
Pixabay licence is permissive but not a free-culture licence, so this
one file is not redistributable under the GPL's terms the way the rest
of the tree is.

## Acknowledgements

Klipp stands on a lot of open-source work. Beyond the crates in the tech stack, it links or builds against, and is grateful to, [FFmpeg](https://ffmpeg.org/) (decode / encode / analysis), the [Vulkan](https://www.vulkan.org/) loader and [wgpu](https://wgpu.rs/) (all GPU work), [whisper.cpp / ggml](https://github.com/ggerganov/whisper.cpp) (speech, via [whisper-rs](https://crates.io/crates/whisper-rs)), [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) (on-screen text), [Piper](https://github.com/rhasspy/piper) and [Kokoro](https://huggingface.co/hexgrad/Kokoro-82M) with [espeak-ng](https://github.com/espeak-ng/espeak-ng) (the narration voices), [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) (Voice Isolation; its inference code is vendored in `crates/klipp-deepfilter` under MIT OR Apache-2.0, with [tract](https://github.com/sonos/tract) running the model), [libfacedetection / YuNet](https://github.com/ShiqiYu/libfacedetection) (faces), [egui](https://github.com/emilk/egui) (UI), [cpal](https://crates.io/crates/cpal) and the [LV2](https://lv2plug.in/) ecosystem via [lilv](https://gitlab.com/lv2/lilv) (audio), [frei0r](https://frei0r.dyne.org/) (video plugins), and [Jamendo](https://www.jamendo.com/) and [Freesound](https://freesound.org/) (the music and sound-effect libraries). Each is used under its own licence. Contributions of missing or incorrect attribution are welcome — open an issue.

