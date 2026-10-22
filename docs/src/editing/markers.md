# Markers

Markers are reference points placed on the timeline to annotate specific moments: beats, sections to review, notes on edit decisions, review comments.

## Adding Markers

Press **M** to add a marker at the playhead. It appears on the timeline ruler. **Shift+M** adds a marker to the selected clip instead, which moves with the clip.

## The Markers Panel

**Window → Markers** lists every marker with its time, name, colour and kind. Click one to go to it. Each marker has:

- **Name** — a short label shown on the ruler
- **Comments** — longer notes
- **Colour** — for sorting by eye
- **Kind** — none, Chapter, To Do, Comment, Best Take, Section or Review

A marker can span a range: **Convert to range marker** gives it a length (five seconds to start); **Clear range** makes it a point again.

## Navigating Between Markers

| Action | Shortcut |
|--------|----------|
| Next marker | Shift+↓ |
| Previous marker | Shift+↑ |
| Go to a marker | Click it in the Markers panel |

## Deleting Markers

**Mark → Delete Marker** removes the marker at the playhead; **Delete All Markers** removes them all. A marker's menu on the ruler also deletes it.

## Review Comments

Review comments travel as timecoded markers:

- **Export Review Copy** writes an MP4 to send for review, and **Export Review List** the comments as a text file.
- **Import Review Comments** reads comments back from a reviewer's list, each as a Review marker with its author.
- **Show only open review comments** hides the ones dealt with.

## Chapters

MP4, MKV and MOV exports carry chapters taken from markers whose kind is **Chapter**. **File → Export YouTube Chapters** (also in the Markers panel) writes the markers as chapter timestamps for a video description.

## Clip Markers and Source Markers

- **Timeline markers** stay at a fixed time on the ruler.
- **Clip markers** are attached to a clip and move with it. Scene detection leaves its cuts as clip markers.
- **Source markers** are on the media itself, added with **M** in the source viewer, along with favourite and rejected ranges; see [Source Viewer](../ui/source-viewer.md#logging-in-the-source-viewer).
