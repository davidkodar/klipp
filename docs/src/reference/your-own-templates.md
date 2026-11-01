# Making Your Own Templates and Recipes

Klipp keeps three kinds of reusable work as files you can make, edit and share:

- **Title templates** (`.klipptitle`): a title whose texts are fields to fill in, such as a name strap or a quote card.
- **Recipes** (`.klipptemplate`): effects built in the node graph, saved as one item with a few controls, such as a twirl or a glow.
- **Motion-graphics templates** (`.klipptemplate`): a whole composition, such as an opener or an end screen, with its texts, colours and media as fields.

The easiest way to make one is in Klipp; the files are JSON and can also be written or edited by hand. Every example on this page is checked by a test that reads it as Klipp does.

## Where they go

Your own are kept in `~/.local/share/klipp/` (or `$KLIPP_DATA_DIR`):

| Kind | Folder |
|---|---|
| Title templates | `title_templates/` |
| Recipes and motion-graphics templates | `templates/` |
| Pictures a template uses | `template_assets/` |

Klipp reads them when it starts and again after you save one. Yours are listed beside Klipp's own and never replace one: saving over one of Klipp's makes a copy with a new id.

**Sharing:** send the file. To share several at once, with their pictures and presets, make a template pack: an installed pack's titles are in its `titles/` folder, its recipes and templates in `templates/`.

**What Klipp refuses:** a file someone else made is read as carefully as one you made. Klipp refuses:
- a link, or a file over 16 MB;
- a file written by a newer Klipp;
- an unknown node or effect, or a plug-in effect in a recipe;
- a value out of range, or past its limits, such as 16 slots or 64 controls.

## Title templates

**Make one:** design the title on the timeline. Then choose **Save as Title Template** from the clip's menu or the inspector. It is listed under Titles in the Library, in the category you gave it.

**The file:**
- `name` and `category`.
- `data`: the title's layers, as a project saves them.
- `design` (`format` 2), which says how the title is used:
  - `anchor`: the point of the frame it is placed by, from 0 to 1 across and down. On a tall or square frame it keeps that place.
  - `fields`: names for its text layers, in stacking order. These are what is filled in each time it is used, by you or the assistant.
  - `accent`: the layer whose colour follows a brand kit's accent.
  - `preview`: the text the Library card shows.

```json
{{#include template-examples/name_strap.klipptitle}}
```

## Recipes

**Make one:** in the node graph, select the nodes that make the effect. Then right-click and choose **Save Node as Recipe…** (or **Save N Nodes as Recipe…**), and pick which of their settings become the recipe's controls. The recipe is listed under Recipes, in the category you gave it, and adds those nodes to any clip in one step.

**The file:**
- `id`: a UUID of its own.
- `name` and `category`.
- `kind`: `recipe` for picture nodes, `audio_chain` for a chain of audio effects, `clips` for an edit made over clips.
- `composition_id`: the nil UUID, for a recipe.
- `parameters`: the controls the inspector shows. Each has a `label` and a `default` (`Number`, `Text`, `Color`, `Bool` or `Enum`), and a `range` or its `choices`.
- `bindings`: which node setting each control drives, as `node:<n>.<setting>`, where `n` is the node's place in `graph.ops`.
- `graph.ops`: the nodes to add, each by its type and settings.
- `slots` (optional): named inputs the recipe asks for, such as a clip, a title or a subject matte.

```json
{{#include template-examples/gentle_twirl.klipptemplate}}
```

## Motion-graphics templates

**Make one:** build the composition from clips and titles, select them and choose **Save as Template…** from their menu. Then choose its fields: the texts, colours, numbers and media that change each time it is used. Its file is a `.klipptemplate` like a recipe's, carrying the composition in `sequences`, with each field a `parameter` bound to a property inside it (for example `object:0.text`). Klipp's own, in the Library's Templates, are good starting points: open one, change it and save it as your own.
