# Changelog

Each release lists what changed for people using Klipp. Versions follow
[Semantic Versioning](https://semver.org/); a `-rc` version is a preview.

## Unreleased

## 0.2.0 — 2026-10-29

This release adds the Composite page, where a clip's picture is built as a
graph of nodes; recipes, ready looks such as sky replacement or a title
behind the subject, with live controls; planar tracking, to pin titles and
pictures to a wall, a screen or a sign; templates with media slots, shared
as one file; and shorts cut from a long video. Most effects, transitions
and audio tools were checked against what their names promise and
corrected where they fell short.

### Added

- The Composite page: a clip's picture graph under the viewer. Merge
  branches of effects with masks, draw titles and pinned pictures inside
  the graph, view any node, wipe between a node's input and output, and
  show one channel of the picture.
- Templates: save clips as a template with media slots and the controls
  they expose, link and offset those controls, and share templates as one
  `.klipppack` file with their pictures, fonts and licences. The built-in
  templates, including a motion pack of twelve, are laid out for wide,
  tall and square frames.
- Recipes: ready-made looks built into a clip's picture, with live
  controls: matted glow, sky replacement, a title behind the subject,
  screen replacement, background replacement from a clean plate, colour
  hold, camera shake, glitch and more. Save your own nodes as a recipe.
- Planar tracking: track a surface from a click, correct it on one frame,
  and pin a title, clip or picture to it. The pinned layer hides where the
  track is lost and takes the surface's light, shade and texture. Ready
  looks put text on a wall, a screen or a sign. Grade windows and
  stabilisation follow the same tracker.
- Shorts: find the moments of a long video that work on their own, and
  build a vertical short from each, from the media pool.
- Export every format at once from a sequence's versions, and rebuild a
  version from its master.
- Metal title faces (gold, silver, steel, copper, bronze, chrome) that
  reflect the shot, also on 3D titles. Titles take picture effects and a
  glow, keep their text readable over busy pictures, and have layouts made
  for 9:16 and 1:1.
- A catalogue of open-licence fonts, fetched on request and previewable in
  the font picker.
- Captions animated word by word from the transcript's timings.
- New effects: Displace, Warp, Gradient Map, flip, a gradient opacity
  fade, reflections in water, on a floor, a wet street or glass, and old
  film ageing. Freeze a frame from the clip menu. The generators add full
  SMPTE bars.
- Take the pauses out of a clip, and set a starting grade with auto
  colour.
- First-time help: a welcome that shows the keyboard layout, a short tour,
  and a hint on the empty timeline.
- Select words in the transcript by dragging across them.
- Export to `.gif` and write 24-bit PCM.
- Narration downloads the Piper speech engine on request.
- Dolby Vision profile 5 clips are flagged at import.

### Changed

- Effects and transitions look as their names say, including glow, lens
  flare, light leak, glitch, ripple, wave, zoom and page turn. Every
  picture effect's amount is on one strength scale, and effect settings
  are held to a safe range.
- Pitch shift, normalize, declick, noise reduction, de-esser and hum
  removal work as named. Reverb and delay tails ring past a clip's end.
- Stabilising keeps a pan or tracking move and takes out only the shake.
- Grading: shadows and highlights make moderate moves, foliage and highlight
  zones change the picture, positive tint goes magenta, and matching one
  shot to another carries its white balance. The Colour page grades the clip
  you mean, with basic correction in its Colour tab.
- Export: the buttons stay in view, progress shows in the panel, the
  default path is beside the project, and the chosen range is kept. An
  export no longer overwrites existing files.
- Placing clips: a sound-only file lands on a free audio track as music
  and ends with the picture above it, with a fade. Place on Top reuses a
  free track and moves the playhead past what it placed, a drop on empty
  track space lays the clip down, and a linked clip's sound moves with its
  picture.
- Import Media opens in the folder the last import came from, and the
  frame-rate question is asked once per project.
- Keymap layouts follow their editors' published defaults and show
  conflicts.
- In a small window, Auto sizes the interface to fit.
- Transcribe names a missing speech model and offers to fetch it, then
  shows its progress and the transcript.
- Reversed clips play smoothly from the GPU decoder.
- Crash recovery is offered on every open when an auto-save is newer than
  the last save.
- Relinking finds media outside a copied project and keeps its analysis.
- Files from other people (projects, templates, subtitles, imports) are
  read within limits, ffmpeg opens local files only, and the control
  socket and downloads are private to the user.

### Fixed

- Undo and redo give back the same track names.
- Inspector fields no longer disappear or lose the keyboard when the panel
  layout changes, and a field left with Enter keeps its letters.
- The viewer's overlays no longer paint over floating windows.
- Skimming the timeline shows the timeline's picture.
- A transition's incoming clip opens from where it first shows.
- ProRes 422 is written as standard 422, and an export says when it
  letterboxes.
- Nested templates no longer hang playback or grow video memory.
- The page switcher stays put when the page changes.
- Klipp no longer aborts now and then on quit with NVIDIA drivers: GPU
  work runs on Vulkan, and on OpenGL only without it.

### Known limits

- Tested on NVIDIA GPUs only; reports from AMD and Intel are welcome
  (#2747).
- Some LV2 and CLAP plug-ins lose internal state, such as a loaded file or
  sample, on save and reopen (#1191).
- Dolby Vision profile 5 clips are flagged at import, but their colours
  are not converted yet (#1224).

## 0.1.0 — 2026-10-26

The first release.

### Added

- Releases are signed. Install Update checks that the release's checksums
  carry Klipp's release signature before it downloads the package, and
  installs nothing otherwise; the package now depends on `minisign`.
- Check an export, or a finished file, against a delivery spec.
- Set up AI assistants from one window, for more of them.

### Changed

- The timeline clip's right-click menu is much shorter: tasks are grouped
  into submenus.
- A right-click on the viewer's picture opens the viewer's menu.

### Fixed

- Muting a track silences it while the timeline plays, and solo plays only
  the soloed tracks; a muted track's button shows it clearly.
- Export renders only the In/Out range when In and Out are set.
- Undo after an Insert that split a clip leaves no extra piece behind.
- A clip dropped below the last track lands on the track made for it.
- A double-click on a media pool item opens it in the source viewer and no
  longer adds it to the timeline.
- Playback no longer stalls at a cut into a title whose transition has a
  matte.
- A project opened with a title, still, generator or compound clip at the
  playhead shows its picture at once.
- Timecodes show frames, not internal ticks, at 23.976, 29.97 and 59.94 fps.
- Overlapping title clips keep their labels apart on the timeline.
- The RGB Curves graph fits inside the inspector.
- The Audio Mixer's strips fit the panel, so every track's mute and solo can
  be reached, and dragging the mixer's top edge sets its height.
- The Audio Mixer shows only tracks that can be heard: no strips for text
  and subtitle tracks, or for a video track whose sound plays from an audio
  track.
- Mixer strips no longer overlap, so each one's level meters and automation
  mode show whole.
- Text and subtitle track headers no longer show mute and solo.

## 0.1.0-rc.3 — 2026-10-25

### Added

- Bins in the media pool: New Bin from the toolbar, the File menu, the command
  palette or a right-click anywhere in the pool; Move to Bin and New Bin from
  Selection for one clip or many; bins are listed first with headers that fold,
  apart from the clips not in a bin.
- Check for Updates shows its answer in the About window, and Install Update
  downloads the new release package from GitHub, checks it against the
  release's checksums, asks, and installs it with pacman.
- Klipp asks once, on first use, whether to check for new versions
  automatically; nothing is checked until you answer. A release-candidate
  build is told about the next release candidate, not only stable releases.
- Follow System Theme follows the desktop's dark or light preference and
  accent colour on any desktop that publishes them, and switches live.
- Tested build steps for Ubuntu 24.04 and Fedora; Klipp builds against
  FFmpeg 6.1 too.

### Changed

- A track's sound effects run once on the track's mixed clips, before its
  fader, the same in playback and export; a limiter there caps the track's
  true peak. Projects with effects on a whole track may sound slightly
  different.
- The main menus and the right-click menus lay their rows out one way: icons and checks in one slot, labels
  aligned, shortcuts in their own column, no wrapped rows.
- The update question and the new-version notice open in the middle of the
  window.

### Fixed

- Opening a project no longer marks it as changed: unsaved changes now mean
  what a save would write differs, and reopening keeps every analysis field.
- Cutting words in the transcript cuts only the speaker's clips and the
  picture linked to them, not every track.
- Playback shows the timeline's frames when a clip has a faster frame rate,
  as export does, instead of every frame of the file.
- Exporting 1080p clips at 4K is many times faster: the GPU enlarges them,
  with the same Lanczos result as before.
- Bypass on a clip's Grade node turns its whole grade off and on, and the
  paused picture follows at once.
- Dragging a point on a hue curve moves only that region, and hue curves join
  smoothly across red.
- The Curve Editor's property buttons switch the curve shown.
- A caption that would sit on a title's line moves below or above it.
- Undo removes a track that the undone edit created.
- A clip dropped from the media pool lands where it is let go, and a drop on
  a bin header no longer adds it to the timeline.
- "Match first clip" takes the clip's exact frame rate on every way of
  placing it.
- Typing on the welcome screen or in a dialog no longer triggers editing
  shortcuts behind it.
- An export preset with its own loudness target keeps its own true-peak
  ceiling, which can now be edited beside the target.

## 0.1.0-rc.2 — 2026-10-24

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
- The colour check names a cast against the look's target, no longer reads
  faces in the background of low-resolution clips, and a clip played to its
  file's end no longer counts as cutting its last word.
- Replies over MCP are shorter (fonts, the colour check grouped by shot,
  cut summaries) and say a change was applied only when one was made.
- LV2 plug-ins that passed the sound through untouched now process it, and a
  plug-in that cannot run is refused when it is added, with the reason.
- Audio plug-ins are applied in exports, loudness measurement and the check,
  not only while playing.
- Shots placed on a beat grid meet, with no frame left between them and no
  place refused as an overlap.
- Fast cuts within one file play every frame, and layered tracks cut between
  files hold fewer decoders and less GPU memory.
- Stacked stills stay on the GPU instead of being uploaded again after the
  first frame.
- Opening video on NVIDIA no longer risks a hang from the VA-API driver.

### Changed

- Picture search and Isolate Object run 4–20× faster on the CPU, and on the
  GPU when a GPU build of ONNX Runtime is installed (onnxruntime-cuda on
  NVIDIA); the AI models panel says which each model uses. Both download new
  model files once, and the old ones are removed.
- Face detection uses the GPU when ONNX Runtime offers one.
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
