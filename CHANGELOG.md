# Changelog

Each release lists what changed for people using Klipp. Versions follow
[Semantic Versioning](https://semver.org/); a `-rc` version is a preview.

## 0.1.0-rc.1 — 2026-10-23

The first public preview of Klipp, a native video editor for Linux.

### What Klipp does

- **Edit** on a magnetic multitrack timeline: ripple and insert edits, J and
  L cuts, synced multicam, a selects timeline, transitions, speed ramps,
  optical-flow slow motion, keyframes with a curve editor, and cuts made by
  editing the transcript.
- **Picture**: one node graph per clip for effects, grades, mattes, keys,
  transforms, LUTs and OFX/frei0r plug-ins, composited on the GPU.
- **Grade** in scene-linear colour: camera-log decoding, wheels and curves,
  HSL, RGB and 3D qualifiers, a grade library, scene matching, GPU scopes, and
  SDR and HDR delivery including HDR10+.
- **Titles**: OpenType text with complex scripts, vertical text, variable and
  colour fonts, animators and per-glyph motions, extruded 3D text, rolling
  credits and a template library.
- **Mix**: multitrack audio with track automation, EQ and dynamics, voice
  isolation, loudness and true-peak delivery, and dual-system sound sync.
- **Captions**: SRT, VTT, ASS, IMSC/TTML, SCC and EBU-STL in and out, CEA-608
  captions embedded in the video, and on-device translation.
- **Exchange**: OTIO, FCPXML, EDL and AAF in and out; FCP7 XML and MLT in;
  Kdenlive out.
- **On-device AI**: transcription, subject masks, face and text detection,
  narration voices, picture search and audio roles, each downloaded only when
  you ask for it, and each with its own switch in Preferences.
- **Assistant**: an MCP server that lets an external assistant analyse
  footage, propose an edit plan, build it, check it and export it.

### Install

- Arch Linux or Omarchy on x86_64, with a working Vulkan driver.
- Download the package from this release and install it with
  `sudo pacman -U ./klipp-0.1.0rc.1-1-x86_64.pkg.tar.zst`. Pacman installs the
  dependencies. Or build from source with `makepkg -si` in `packaging/`; see the
  [installation guide](packaging/README.md).

### Known limitations

- This is a preview. Expect rough edges, and keep your own copies of work you
  care about: the project format can still change between previews, and a
  newer Klipp is not guaranteed to open a project saved by an older one.
- GPU support is tested mainly on NVIDIA. AMD and Intel GPUs should work
  through Vulkan but are not yet validated.
- X11 sessions are less tested than Wayland.
- The plug-in hosts cover subsets of OFX, frei0r, LADSPA, LV2 and CLAP;
  compatibility varies by plug-in. VST2 and VST3 are not supported.
- There is no signed package repository yet, so updates are installed by
  downloading the new package; the in-app updater needs that repository.
