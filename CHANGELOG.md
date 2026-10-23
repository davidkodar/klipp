# Changelog

Each release lists what changed for people using Klipp. Versions follow
[Semantic Versioning](https://semver.org/); a `-rc` version is a preview.

## Unreleased

### Added

- Lower thirds have a backing (a shadow and outline by default, or a box) so
  they read over any picture.
- Caption translation keeps the brief's names and terms as written, and takes
  a glossary for recurring terms.
- The assistant can duplicate a sequence and export the timeline (FCPXML,
  EDL, OTIO, AAF or Kdenlive) over MCP. As from the menu, an export carries
  the edit, not grades, most effects or most titles.
- Opening a project says how many media files are offline, and media can be
  relinked from a whole folder at once.
- The assistant can export HDR (HLG or PQ), make proxies, lock tracks, and
  look at frames at full size over MCP.

### Fixed

- Captions are made only from the speech you hear: each word once, from the
  multicam angle whose sound plays, without music, effects or muted clips,
  with no cue too short to read, and in the transcript's language.
- Editing a multicam clip keeps every angle where it was cut: ripple trims
  and deletes move all angles together, and splits, inserts, overwrites,
  lifts, rolls, slides, slips, joins and head trims keep each angle at its
  moment.
- The check reads only what the program shows, so hidden multicam angles no
  longer give false colour errors or repeat a finding once per angle.
- Contact sheets name the multicam angle that is on screen.
- Find by Picture can be opened from the media pool in either layout and from
  the command palette.
- Menu and tooltip shortcuts (viewer overlays, storylines) show the keys you
  have bound.
- Preferences and scope texts describe Klipp's own features without naming
  other editors.
- Exporting without burnt-in captions keeps titles such as lower thirds.
- Captions on a vertical frame are larger and higher, and refit adapts them.
- Refit keeps a clip's own reframe, and on a screen recording it follows what
  changes on screen rather than the face camera.
- Listing clips over MCP gives each caption's and title's full text.
- The reverb keeps a sound near its own level; it used to raise it by about
  17 dB and could clip the export.
- FCPXML export writes every picture and sound track and constant speed
  changes, and lists what the format leaves out (grades, effects,
  transitions, captions, title looks, fades) beside the file.
- Importing a timeline reuses media already in the project, gives the new
  sequence a name of its own, and reports clips that run past their media.
- A ripple retime moves later clips' linked sound with them, so picture and
  sound stay in sync.
- Narration takes keep headroom (-1 dBTP) and lose the synthetic voice's
  low rumble.
- The loudness check measures the whole mix against the delivery, and an
  export with a preset keeps the brief's loudness unless told otherwise.
- OFX plug-in parameters with several values (colours, points) are read and
  set correctly.
- A project holding the same clip id twice is repaired when saved.
- An edit that is refused says so, and why, instead of reporting success.
- Large phone photos (JPEG stills) show in the viewer and are analysed for
  the people and faces in them.
- Grading measurements read the clip's own picture, not titles drawn over it
  or the blurred fill in its bars, and matching a grade stops before it clips
  the highlights.
- Every line of a title gets its backing, and a lower third can keep inside
  the picture of a pillarboxed shot.
- Dissolves and dips onto or off a still need no extra footage.
- A misspelt audio effect setting is refused with the valid names.
- Exports to ProRes, DNxHD or with letterboxing are composited on the GPU.
- Captions use the brief's spellings in every transcript, avoid one-word
  cues, and translation keeps whole sentences; selects on a take with no
  speech are made from its shots.
- A lower-frame-rate clip can be conformed by blending or optical flow at
  full speed, and the check notes clips that will judder.
- EDL and Kdenlive exports list what they leave out, as FCPXML and OTIO do.

### Changed

- Builds without warnings on Rust 1.99.
- The user guide, README and website describe the app as it is today, and
  point to the release download.
- The README and feature reference say what timeline interchange carries:
  the edit (clips, cuts, timing, and markers where the format has them), not
  grades, most effects or most titles.

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
