# Introduction

Klipp is an in-progress non-linear video editor for Linux, written in Rust with an egui/eframe interface. This documentation describes what is currently wired up — not what works reliably, not what is feature-complete, and not what should be relied on for real work.

## Status

🚧 **Early development.**

Many subsystems are still changing. There is no stability commitment, no release schedule, and no support. Release candidates are published as an unsigned Arch package; otherwise Klipp is built from source. You should expect to hit rough edges, breakage, and incomplete features.

## What's Wired Up

The list below describes which subsystems currently produce output. None of these are claims of quality, completeness, or production readiness.

### Timeline editing
- Multi-track video, audio and subtitle tracks, with a magnetic model (insert / overwrite, ripple, snap)
- Three- and four-point editing: insert, overwrite, fit to fill, place on top, replace, backtiming
- Compound clips, multicam clips, adjustment layers and connected clips
- A selects timeline stacked above the cut
- Undo / redo, with project-wide steps kept in a history of their own

### Titles and graphics
- Titles with OpenType shaping, complex scripts, vertical text, variable fonts and colour emoji
- Animators, keyed text properties, text on a path, gradients, 3D extruded text
- Title templates, rolling credits and motion-graphics templates with editable fields
- Generators (solids, bars, gradients, timers, slates, shapes) and piece transitions

### Colour pipeline
- A node graph per clip, per track, per grade group and for the whole timeline
- Lift / Gamma / Gain / Offset wheels, log wheels, HDR Zones, curves and qualifiers (HSL, RGB, 3D)
- Working colourspaces Rec.709, Rec.2020 and ACEScg, linear-light throughout
- Camera log detection and decode (Sony S-Log3, Canon Log, Panasonic V-Log, Apple Log, ARRI LogC, RED Log3G10, Leica L-Log and more)
- Freeform and shape masks, keyed over time, and tracked mattes
- Waveform, vectorscope, histogram and parade scopes drawn on the GPU

### Effects
- Built-in effects as GPU compute nodes (wgpu / Vulkan), with CPU kernels as fallback, particles (dust, snow, rain, sparks, embers) among them
- OFX and frei0r plug-ins, run as nodes of their own
- Keyframes with easing presets and per-key handle kinds, in a Curve Editor and a Dope Sheet

### Audio
- Per-clip volume, pan, EQ and fades; per-track volume and pan automation
- EBU R128 loudness metering and normalisation to a loudness or peak target
- LV2 audio effects, a denoiser and optional voice isolation
- Output through cpal — PipeWire / ALSA / JACK

### Media and decode
- CUDA NVDEC and VAAPI hardware decode, with software decode when the GPU is busy or refuses
- Proxies, skim files, metadata from cameras and sidecars, smart bins
- Transcripts (whisper.cpp) and search by words or by what is in the picture

### AI tools
- Remove Background, Isolate Object and Detect Objects, all on-device
- Track Subject, scene detection, shot and sound classification
- Models download on request; nothing is bundled

### Export
- H.264, H.265, AV1, VP9, ProRes, DNxHD and animated GIF; sound alone as MP3, WAV or M4A
- HDR10 / HLG output, HDR10+ metadata on H.265 PQ
- Captions burnt in, as sidecar files, or as CEA-608 in the video stream
- A delivery check over picture and sound before export
- Timeline interchange in and out: FCPXML, Final Cut Pro 7 XML (in), CMX 3600 EDL, OpenTimelineIO, AAF (through OpenTimelineIO) and Kdenlive / MLT

### Assistant
- A local control socket and an MCP server, so an assistant can read and edit the open project

## What's Missing

A non-exhaustive list:

- Reliability — no extended soak-testing has been done
- AMD and Intel GPU acceleration for AI inference
- Signed packages and installers for distributions other than Arch
- Documentation accuracy under continued churn (this doc may drift)

## Author and Licence

Klipp is developed by David Bolin and released under the GNU General Public Licence v3 (GPL-3.0). GPL-3.0 is compatible with code under permissive licences (MIT, Apache-2.0); bundled assets such as Phosphor icons (MIT) and the Film Emulation LUT collection (MIT) are redistributed under their original terms.
