# Media Browser

The media browser manages all imported media and provides tools for organising, previewing, and adding clips to the timeline.

## Views

Toggle between two view modes using the icons at the top of the browser:

### Grid View
Displays media as thumbnail tiles in a grid layout. Tiles show the first frame of video clips and a waveform preview for audio files. Hover over a tile to skim through it.

### List View
Displays media in a sortable table with columns for name, duration, resolution, codec, frame rate, and file size. Click column headers to sort.

## Skimming

Hover over any clip thumbnail in grid view to skim through its contents. The thumbnail updates in real time as you move the cursor horizontally across it, giving you a quick preview without committing the clip to the source viewer or timeline.

Skim is decoder-throttled: the underlying decoder runs sparse keyframe decode at ~30 Hz with coalesced seeks, so skimming a 4K bin stays responsive.

## Bins

Organise media into bins (folders) for large projects:

- Right-click in the browser to create a new bin
- Drag media into bins to organise
- Double-click a bin to navigate into it
- Use the breadcrumb bar at the top to navigate back

## Selection

- **Click** — Select a single item
- **Shift+Click** — Range selection (select all items between the last selected and clicked item)
- **Ctrl+Click** — Toggle selection of individual items
- **Ctrl+A** — Select all items in the current bin

## Sorting

Sort media by:

- Name (alphabetical)
- Duration
- Date imported
- File size
- Resolution

Click the sort button or column headers to change sort order.

## Search

The search box at the top of the browser matches against:

- File name
- User-applied tags
- Bin path

Searches are sub-millisecond — Klipp builds an in-memory index when the project loads.

## Smart Bins

Smart bins automatically collect media matching certain criteria:

- By media type (video, audio, image)
- By resolution (4K, 1080p, 720p)
- By frame rate

Smart bins update dynamically as you import new media.

## Adding Media to Timeline

From the browser, add media to the timeline by:

- **Double-click** — Load into the source viewer for marking up
- **W** — Insert at playhead (with browser clip selected)
- **D** — Overwrite at playhead
- **Drag and drop** — Drag directly onto a timeline track

## Effects Browser

Switch to the effects browser tab to browse and apply effects. Effects are categorised by type (colour, blur, distortion, etc.) and can be dragged onto clips or applied from the inspector.

## Title Browser

The title browser lists generated title presets — lower-thirds, full-screen titles, end-card layouts. See [Titles](../editing/titles.md) for the full reference.
