# klipp-mcp

An MCP server that lets an agent drive a running Klipp: inspect the
timeline, move the playhead, apply grades, and look at the actual viewer
output.

## How it fits together

```
MCP client (Claude Code, ...)
      | MCP: JSON-RPC 2.0 over stdio
klipp-mcp
      | line-delimited JSON over a Unix socket
klipp-app (klipp-control server thread)
      | AppCommand channel + shared state
engine / UI
```

Reads (status, clips, grade graph, screenshot, perf, log) go straight at
the state the UI and engine already share. Writes go through the same
`AppCommand` channel the UI's own buttons use, so undo, autosave and the
engine's caches behave exactly as they do for a human clicking.

## Build and register

```bash
cargo build --release -p klipp-mcp
```

The repo ships a project-scoped `.mcp.json`, so a client started in this
directory picks the server up with no further setup. To register it
globally instead:

```bash
claude mcp add klipp -- /absolute/path/to/target/release/klipp-mcp
```

Then start Klipp. The control socket appears at
`$XDG_RUNTIME_DIR/klipp-control.sock` once the app is up; every tool
reports clearly if it cannot find it.

## Tools

| Tool | What it does |
|---|---|
| `klipp_status` | Transport state, timeline summary, viewer resolution, latest perf sample |
| `klipp_play` / `klipp_pause` / `klipp_stop` | Transport |
| `klipp_seek` | Absolute seek in seconds |
| `klipp_list_clips` | Tracks and clips: ids, timings, media, whether the grade is an identity |
| `klipp_list_media` | The media pool |
| `klipp_import_media` | Import a file (asynchronous — poll `klipp_list_media`) |
| `klipp_append_clip` / `klipp_remove_clip` / `klipp_select_clip` | Timeline editing |
| `klipp_get_grade` | A clip's colour node graph as JSON, with node ids |
| `klipp_add_node` / `klipp_remove_node` / `klipp_connect_nodes` | Build a grade graph |
| `klipp_set_node_param` | Set one numeric grade parameter |
| `klipp_toggle_node` | Bypass a node |
| `klipp_set_log_format` | Declare a clip's camera log / HDR encoding (S-Log3, V-Log, LogC3, …) when the file doesn't say |
| `klipp_screenshot` | PNG of the current viewer frame, downscaled by default |
| `klipp_perf` | Frame timings, drops, GPU→CPU readback counters |
| `klipp_log_tail` | Tail of `klipp-debug.log` |

## Notes

* **Commands are queued, not synchronous.** A write returns
  `{"accepted": true}` once it is on the engine's channel. To observe the
  effect, poll `klipp_status` or `klipp_get_grade`.
* **Single instance.** The socket is one well-known path; a second Klipp
  takes it over. Set `KLIPP_CONTROL_SOCKET` to run two side by side.
* **Disable** the control surface entirely with `KLIPP_CONTROL=0`.
* The socket lives in `$XDG_RUNTIME_DIR`, which is user-owned and mode
  0700, so it is reachable by the user running Klipp and nobody else.
