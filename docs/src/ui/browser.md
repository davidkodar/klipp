# Media Browser

The media browser manages all imported media and provides tools for organising, previewing, and adding clips to the timeline.

## Views

Toggle between two view modes using the icons at the top of the browser:

### Grid View
Displays media as thumbnail tiles in a grid layout. Tiles show the first frame of video clips and a waveform preview for audio files. Hover over a tile to skim through it.

### List View
Displays media in a table. A header names the columns: click one to sort by it, click again to flip the order, and right-click the header to choose which columns show, from duration, resolution, frame rate, codec and size to camera, lens, timecode, reel, shoot date, scene, shot, take, keywords, description, comments, good take, label, tags, favourite, rejected and markers. The choice is kept in the preferences. Each part of a split take is named "part 1 of 3" after its file name.

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

Sort media by any sortable field: name, date added, shoot date, duration, rating, codec, size, resolution, frame rate, camera, lens, timecode, reel, scene, shot, take, keywords or kind. Numbers sort as numbers, so take 2 comes before take 10. Use the sort dropdown or the list's column headers.

## Search

The search box at the top of the browser matches against:

- File name and bin path
- User-applied tags
- Everything logged: scene, shot, take, description, comments, keywords, reel
- Camera model and lens

Searches are sub-millisecond — Klipp builds an in-memory index when the project loads.

## Filters

The filter row narrows the bin: a fixed filter (unused, video, audio, stills, offline, favourites, rejected), a minimum rating, a colour label, and one metadata value from whatever the pool holds — codec, frame rate, camera, lens, reel, scene, keyword, resolution, or good takes only. Filters combine.

## Smart Bins

A smart bin is a set of rules you write, saved with the project: **New smart bin…** in the filter dropdown opens the editor. Each rule is a field, a comparison and a value — *Rating is at least 4*, *Keywords contains interview*, *Shot on is after 2024-03-01*, *Camera is FX6*, *Resolution is at least 4k*, *Duration is at most 1:30*, *Good is set* — and the bin gathers the clips matching all of them or any. Numbers compare as numbers, dates as dates, text as text. Pick a saved bin from the same dropdown; edit or delete it there. Smart bins update as media is imported and logged.

## Logging

The Info tab's **Logging** section holds the editor's record of a clip: scene, shot, take, reel, keywords, description, comments and a good-take mark. Everything there is a column, a sort key, a filter and a smart-bin field, and goes out and comes back in with the metadata sheet (see [Importing Media](../editing/importing.md#metadata-sheets)).

## Marks on a Clip

With the source viewer active, **M** leaves a note on the pool clip at the source playhead, **Shift+F** marks the stretch between the in and out points as a favourite and **Shift+R** rejects it; with no points set the mark covers the whole clip. The Mark menu has the same. Marks show on the source scrub bar, are named and removed in the Info tab's **Markers** section, and the browser's Favourites and Rejected filters gather clips by them.

## Synced Sound

Select a camera clip and a separate recording, right-click, **Sync Video and Audio**. When both files carry a start timecode the offset is taken from them; otherwise the two sounds are matched by waveform. The pairing is kept on the clip: whenever it is added to the timeline the recording goes under it at that offset in place of the camera's own sound. **Remove Synced Audio** undoes it.

## Adding Media to Timeline

From the browser, add media to the timeline by:

- **Double-click** — Load into the source viewer for marking up
- **,** — Insert at playhead (with a browser clip selected)
- **.** — Overwrite at playhead
- **Drag and drop** — Drag directly onto a timeline track

## Effects Browser

Switch to the effects browser tab to browse and apply effects. Effects are categorised by type (colour, blur, film, distortion, etc.) and can be dragged onto clips or applied from the inspector. Every card is rendered by the real effect over a reference frame.

The source filter above the list defaults to **Built-in**: Klipp's own GPU
effects, the ones that run at full speed in the viewer and the export.
**Installed** shows the frei0r and OpenFX plugins found on the system; they
run on the CPU and are kept as slow alternatives. **All** lists both.

The **Looks** category holds chains of built-ins applied in one go (Cinematic,
Dreamy, Punchy, Bleach Bypass, Vintage, Anamorphic). Dropping or double-clicking
a Look adds every effect in the chain to the clip, each as its own row in the
inspector, so any part of it can be tuned or removed afterwards.

## Title Browser

The title browser lists generated title presets — lower-thirds, full-screen titles, end-card layouts. See [Titles](../editing/titles.md) for the full reference.
