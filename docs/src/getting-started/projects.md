# Projects & Templates

## The Welcome Screen

Klipp starts on the welcome screen (unless **Preferences → General → On launch** says otherwise). It shows:

- **Recent** and **Pinned** projects, and **All Projects** in your projects folder, named by their file, with their size and when they changed
- **Start from a template** — the built-in templates and your own
- **New Project**, **Open**, and **Import Media**
- **What's New** in this build

A project's menu pins it to the top, renames, duplicates, shows it in the file manager, removes it from the recent list, or moves it to the trash. **Projects Folder** sets where new projects go and which folder the screen lists.

## Creating a Project

**New Project** asks for:

- **Name** and **Location**
- **Resolution** and **Frame rate**, or **Match first clip** to take them from the first clip added
- Or a destination from the same list: **YouTube**, **YouTube Shorts / Reels / TikTok**, **Square post** or **Podcast**. It sets the size and frame rate. It also writes the platform, its loudness target (-14 LUFS, or -16 for a podcast) and whether captions are expected into the sequence's brief. **Window → Delivery Check** and the export then follow the brief.
- With a template, the template's fields, and whether to **include starter content**

A project is a `.klipp` directory; see [Project Format](../reference/project-format.md). A project can also be opened from inside its folder.

## Project Templates

A template keeps a project's structure — its sequences, tracks, titles with their fields, and settings — to start new projects from.

- **File → Save Project as Template...** saves the open project as one, optionally with its media copied into the template as starter content.
- A template's fields — an episode title, a brand colour, a font — are asked for when a project is made from it, and can be the same for every episode or set each time.
- The New Project dialog's template list can **Import...** and **Export...** templates as files, and delete your own.
- A project made from a template opens as an untitled project in the projects folder.

## Opening Other Editors' Projects

**File → Open Project from Another Editor...** opens an FCPXML, Final Cut Pro 7 XML, EDL, OpenTimelineIO, AAF or Kdenlive timeline as a new project. See [Timeline Interchange](../export/interchange.md).
