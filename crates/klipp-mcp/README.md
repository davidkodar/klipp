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

## Connect an assistant

An installed Klipp has the server at `/usr/bin/klipp-mcp`. The easiest
route is **Preferences → AI → Connect an assistant**, which finds the
program, connects Claude Code or Codex with their own commands, checks
the connection, and gives settings for other assistants.

By hand, register the installed command with the assistant:

```bash
# Claude Code, for every project
claude mcp add --scope user klipp -- /usr/bin/klipp-mcp
# Codex
codex mcp add klipp -- /usr/bin/klipp-mcp
```

Any other assistant that runs local (stdio) MCP servers takes this in its
settings:

```json
{ "mcpServers": { "klipp": { "command": "/usr/bin/klipp-mcp", "args": [] } } }
```

Start a new session in the assistant afterwards so it loads the tools.
Klipp must be running with **AI assistant (MCP)** on in Preferences → AI.
The control socket is at `$XDG_RUNTIME_DIR/klipp-control.sock`, and every
tool says clearly when it cannot reach it.

`klipp-mcp --check` asks the running Klipp for its status, prints it in
words, and exits 0. When Klipp is not there it says so and exits 1.

**Online assistants (ChatGPT in the browser) are not supported yet.** They
need an MCP endpoint they can reach over the internet. The control socket
must never be exposed to the network, so this needs an authenticated,
opt-in bridge, which Klipp does not have yet.

### From a checkout

```bash
cargo build --release -p klipp-mcp
```

The repository's `.mcp.json` registers `./target/release/klipp-mcp` for an
assistant started in the checkout. Rebuild that binary after changing
`crates/klipp-mcp` or `assets/craft`, which it embeds.

## Tools

| Tool | What it does |
|---|---|
| `klipp_status` | Transport state, timeline summary, viewer resolution, latest perf sample |
| `klipp_play` / `klipp_pause` / `klipp_stop` | Transport |
| `klipp_seek` | Absolute seek in seconds |
| `klipp_list_clips` | Tracks and clips, a clip to a line: id, kind, range, name, media, and speed, off, graded or effects where set; narrowed by track, time or media; `full` for every field |
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
* **Disable** the control surface entirely by turning off **AI assistant
  (MCP)** in Preferences → AI (from the next start), or with `KLIPP_CONTROL=0`.
* The socket lives in `$XDG_RUNTIME_DIR`, which is user-owned and mode
  0700, so it is reachable by the user running Klipp and nobody else.
