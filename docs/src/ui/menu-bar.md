# Menu Bar Reference

The menus and their items. Shortcuts are Klipp's default keymap; a different preset or your own changes in **Preferences → Keyboard** show in the menus as they are.

## File

| Item | Shortcut | Description |
|------|----------|-------------|
| New Project... | Ctrl+N | Create a project, empty or from a template |
| Open Project... | Ctrl+O | Open a `.klipp` project |
| Open Project from Another Editor... | — | Open an FCPXML, Final Cut Pro 7 XML, EDL, OpenTimelineIO, AAF or Kdenlive timeline as a new project |
| Open Recent | — | Reopen a recent project |
| Save / Save As... | Ctrl+S / Ctrl+Shift+S | Save the project, or save it somewhere new |
| Save Project as Template... | — | Keep the project's structure as a template |
| Revert to Saved | — | Drop unsaved changes |
| Import Media... | Ctrl+I | Import video, audio or image files |
| Import Subtitles... | — | Read SRT, WebVTT, ASS/SSA, TTML/IMSC, SCC or EBU-STL into a subtitle track |
| Import Metadata Sheet... | — | Read clip metadata from a CSV or ALE sheet |
| Import Timeline | — | Kdenlive, EDL, OpenTimelineIO or FCPXML into the open project |
| Export... | Ctrl+E | Open the export settings |
| Export Subtitles... | — | Write a subtitle track as SRT, WebVTT, ASS, SSA, TTML (IMSC), SCC or EBU-STL |
| Export YouTube Chapters | — | Write the markers as a chapter list |
| Save Render Script | — | Write the export as a shell script |
| Export Timeline | — | FCPXML, Kdenlive, EDL, OpenTimelineIO or AAF |
| Export Metadata Sheet | — | Clip metadata as CSV or ALE |
| Close Project | Ctrl+W | Close the project and return to the welcome screen |
| Project Properties... | — | Name, colour management and other project settings |
| Sequence Settings... | — | Resolution, frame rate and audio of the open sequence |
| Last Import Report… | — | What the last timeline import carried, changed or left out |
| Relink Media... | — | Point missing media at its files |
| Consolidate Project... | — | Copy the project's media into one place, optionally only what the cut uses, with the fonts its titles use |
| Quit | Ctrl+Q | Exit Klipp |

## Edit

| Item | Shortcut | Description |
|------|----------|-------------|
| Undo / Redo | Ctrl+Z / Ctrl+Y or Ctrl+Shift+Z | Undo or redo the last step |
| History... | — | Every step, to undo or redo to any point |
| Cut / Copy / Paste | Ctrl+X / Ctrl+C / Ctrl+V | Clips to and from the clipboard |
| Paste Insert | — | Paste, rippling the clips after the playhead |
| Copy / Paste Title Style | — | A title's style onto other titles |
| Copy / Paste Attributes... | Ctrl+Shift+C / Ctrl+Shift+V | Effects, grade, transform and other attributes onto other clips |
| Duplicate | Ctrl+D | Duplicate the selected clips |
| Delete | Delete | Delete the selected clips |
| Select All / Deselect All | Ctrl+A / Ctrl+Shift+A | |
| Select Forward / Backward | — | Every clip after or before the playhead |
| Invert Selection | — | Every clip not selected now |
| Select Same Source | — | Every clip from the selected clips' media files |
| Select by Colour | — | Clips by their colour label |
| Go to Next Gap | — | Move the playhead to the next gap |
| Select All / Forward / Backward on Track | Ctrl+Alt+A / Ctrl+Alt+→ / Ctrl+Alt+← | The same, on one track |
| Preferences... | Ctrl+, | Open the preferences window |

## Clip

| Item | Shortcut | Description |
|------|----------|-------------|
| Enable / Disable | — | Switch the clip off without removing it |
| Solo | — | Hear and see only this clip |
| Match Frame / Match Frame Reverse | F / Shift+F | The source frame of the clip under the playhead, or back |
| Split at Playhead | Ctrl+B | Split the selected clips |
| Close Gap | — | Close the gap before the clip |
| Ripple Delete | Delete | Delete and close the gap |
| Speed... | — | Speed, reverse and retiming |
| Motion Presets | — | Ken Burns zooms and pans, picture-in-picture corners |
| Copy / Paste Attributes | Ctrl+Shift+C / Ctrl+Shift+V | |
| Create Multicam Clip | Alt+C | Group synced angles into one multicam clip |
| Switch Angle | 1–9 | The multicam angle shown |
| Multicam Sound | — | Which angle the sound comes from |
| Normalize Audio… | — | Bring the selected clips to a loudness or peak target |
| Add Track Point / Analyze Track Points | — | Track a point through the clip |
| Reveal in Browser | — | Select the clip's media in the browser |
| Open in Colour Panel | C | Grade the clip |

## Timeline

| Item | Shortcut | Description |
|------|----------|-------------|
| Add Video Track / Add Audio Track / Delete Track | — | |
| Insert Edit | , or F10 | Insert the source range at the playhead, rippling later clips |
| Overwrite Edit | . | Overwrite at the playhead |
| Fit to Fill | Shift+F11 | Retime the source to fill the timeline range |
| Place on Top | F12 | Put the source on the first free track above |
| Replace Edit | Shift+F12 | Replace the clip under the playhead |
| Ripple Overwrite | — | Overwrite, then ripple to the new length |
| Lift Edit / Extract Edit | ; / ' | Remove the in–out range, leaving or closing the gap |
| Freeze Frame | Alt+H | Hold the frame under the playhead |
| Join Through Edit at Playhead | — | Rejoin a clip split at the playhead |
| Render In to Out / Render Timeline / Render Selection | — | Render whole frames, layers and transitions included, into the render cache |
| Clear Render Cache In to Out / Clear Render Cache | — | Drop rendered frames |
| Detect Scene Cuts | — | Split the selected clip where its picture cuts |
| Add Adjustment Layer | — | An effects layer over the tracks below |
| Add Generator | — | Solid colour, colour bars or black |
| Create Compound Clip / Create Multicam Clip | — | |
| Snapping | N | Snap to edits, markers and the playhead |
| Linked Selection | — | Picture and sound trim and move together |
| Fit to Window / Zoom to Selection | Z / Shift+Z | |
| Leave Compound Clip | Ctrl+↑ | Back to the timeline holding the compound |

## Mark

| Item | Shortcut | Description |
|------|----------|-------------|
| Set In Point / Set Out Point | I / O | |
| Clear In Point / Clear Out Point | Shift+I / Shift+O | |
| Clear In and Out | Alt+X | |
| Go to In Point / Go to Out Point | — | |
| Play In to Out | Ctrl+Shift+Space | |
| Add Marker | M | A marker at the playhead |
| Edit Marker... / Delete Marker / Delete All Markers | — | |
| Add Source Marker | M (in the source viewer) | A marker on the source media |
| Favourite / Reject Source Range | Shift+F / Shift+R (in the source viewer) | Rate the source range |
| Next Marker / Previous Marker | Shift+↓ / Shift+↑ | |

## View

| Item | Shortcut | Description |
|------|----------|-------------|
| Zoom In / Zoom Out | = / - | Timeline zoom |
| Multicam Angles | Shift+Alt+M | Show the angles in the source viewer |
| Interface Size | — | Automatic, or a fixed UI scale |
| Viewer Zoom | — | Fit, or a fixed zoom |
| Fullscreen Viewer | F11 | |
| Zebras / False Colour | Alt+Z / Alt+F | Exposure overlays |
| Palette | — | The false-colour palette, and an editor for it |
| Focus Peaking / Safe Areas / Grid Overlay / Checkerboard | — / Alt+S / Alt+G / — | Viewer overlays |
| Scopes | Alt+V | Waveform, vectorscope, histogram or parade; overlaid on the viewer or apart |
| Colour Panel | C | |
| Extract Frame to File… / Extract Frame to Bin | Ctrl+Alt+S / Ctrl+Alt+B | Save the viewer's frame |
| Skimming | S | Skim the timeline and browser with the pointer |
| Dual Viewers | — | Source and program side by side, remembered per workspace |
| Performance Overlay | Ctrl+Shift+P | Decode, GPU and frame-timing stats; Ctrl+Alt+P records a session |

## Window

| Item | Shortcut | Description |
|------|----------|-------------|
| Workspaces | — | Edit, Colour, Export, Audio Mixing, and layouts you save |
| Save Workspace... | — | Keep the current layout |
| Nodes | — | The clip's effects and grade as a node graph |
| Inspector / Browser | Ctrl+Shift+I / Ctrl+Shift+B | |
| Curve Editor | Shift+K | Keyframe curves and the Dope Sheet |
| Audio Mixer | Ctrl+Shift+M | |
| History / Markers / Edit List | — | |
| Subtitle Editor | Ctrl+Shift+T | |
| Transcript | — | The words of the selected clip or the timeline |
| Project Notes | Ctrl+Shift+N | |
| Edit Plan | — | An approvable plan of edits from the assistant |
| Colour Check / Delivery Check | — | Measured checks of the grade, and of picture and sound before delivery |
| Sound Over Time | — | A file's sound as frequency against time, with its faults named |
| Narrate | — | A voice-over made from text on this machine |
| AI Tools | Alt+A | |
| Plugins | — | |
| Command Palette | Ctrl+K | |

## Tools

| Item | Description |
|------|-------------|
| Audio Plugins… | Scan and manage LV2 plug-ins |
| AI Models… | Download, check and remove models |
| Cache Manager… | See and clear caches |
| Render Queue | Queued and running exports |
| Project Doctor… | Find and fix problems in the project |
| System Probe… | A snapshot of the GPU, FFmpeg, AI runtime and OS for a bug report |

## Help

| Item | Description |
|------|-------------|
| Klipp Documentation | Open this documentation |
| Keyboard Shortcuts | The shortcuts reference |
| Report a Bug... | Open the issue tracker |
| Release Notes | What changed in this build |
| About Klipp | Version and licence information |
