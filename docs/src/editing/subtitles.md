# Subtitles & Captions

Captions live on **subtitle tracks**: each caption is a clip with its text, timed on the timeline like any other clip. A sequence can carry several subtitle tracks, one per language.

## Making Captions

- **File → Import Subtitles...** reads SRT, WebVTT, ASS / SSA, TTML (including DFXP, IMSC and iTT), Scenarist SCC (CEA-608) and EBU-STL, keeping their inline styling.
- **Captions from Transcript…** (Subtitle Editor or command palette) makes captions from the transcript of the cut, in to out or the whole sequence, with a chosen number of lines and characters per line; it can replace captions already on the track. See [AI Tools](../ai-tools/overview.md#more-on-device-tools).
- The Subtitle Editor's **+** adds a caption at the playhead on the track.

## The Subtitle Editor

**Window → Subtitle Editor** (Ctrl+Shift+T) lists each track's captions with their times and text. Edit the text in place, click a caption to go to it, and set the track's caption style.

## Caption Style

Each subtitle track has a caption style: font, size, colour, an outline, a shadow or a backing, and the height on the picture. **Save and Apply to All** restyles every caption on the track; new captions on the track take the style.

## Language and Translation

A subtitle track's header menu sets its **Language**, which is written to subtitle exports. **Translate Captions** translates a track into another language on this machine, with a translation model downloaded on request; the translation lands on a new subtitle track under the source, hidden at first, as one undo step. Its progress is on the status bar, and it can be cancelled from the track's menu. Any subtitle track can be shown or hidden.

## Delivering Captions

- **File → Export Subtitles...** writes a track as SRT, WebVTT, ASS, SSA, TTML (IMSC 1.1 Text), SCC (CEA-608) or EBU-STL.
- The export settings can **burn subtitles into the video**, or carry them as **closed captions in the video (CEA-608)** for H.264 and H.265 exports.
- Find and replace reaches subtitles too: **Find and Replace in Titles…** with the subtitles included.
