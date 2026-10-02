# MCP catalog benchmark

Measures whether a change to `klipp-mcp`'s tool catalog makes an agent
better or worse at real edits, and what it costs (#688). The same agent,
prompts and fixtures run against two (or more) builds of `klipp-mcp`.
Each run's final timeline is scored over the control socket, to the frame.

## What it measures

- **Per catalog:** `tools/list` size (`catalog.py`), and what the catalog
  really costs an agent in tokens, by slices through Claude Code
  (`measure_slices.py`, with the stand-in server `fake_mcp.py`). Claude
  Code truncates a tool description at about 2,048 characters, so bytes
  alone mislead: measure tokens.
- **Per task:** correctness against the expected clips (file, timeline
  in/out, source in/out, in frames), tool calls, failed calls, turns,
  wall time, input/output tokens and cost (`bench.py`).

The scenarios in `bench.py` cover:
- frame-accurate cuts, trims (including a slip) and short assemblies
  (an overlay without its sound, a dissolve);
- a less common tool (a marker);
- a rule that lives in an input field (a captions track).

Each states its prompt, starting timeline and pass condition.

## Running it

1. **Fixtures:** `./make_fixtures.sh`. It writes three synthetic 25 fps
   clips with their frame numbers burned in.
2. **The catalogs:** build `klipp-mcp` at each commit to compare into
   `bin/klipp-mcp-<name>`. Use a separate target directory, so the
   `klipp-mcp` you use is not replaced:

   ```sh
   git worktree add /tmp/mcp-pre <commit>
   (cd /tmp/mcp-pre && CARGO_TARGET_DIR=/tmp/mcp-bench-target cargo build --release -p klipp-mcp)
   cp /tmp/mcp-bench-target/release/klipp-mcp bin/klipp-mcp-pre
   ```

3. **A test Klipp:** copy a release `klipp-app` to `bin/klipp-app` (or set
   `KLIPP_BENCH_APP`) and start it with `./run_klipp.sh`. It listens on
   `$KLIPP_BENCH_SOCKET` (default `$XDG_RUNTIME_DIR/klipp-bench.sock`),
   never on the socket of the Klipp you edit in.
   - The window must be rendered for edits to apply. On Hyprland, start
     it on a headless output (`hyprctl output create headless`) and
     remove that output afterwards.
4. **Measure:**

   ```sh
   python3 catalog.py bin/klipp-mcp-pre bin/klipp-mcp-post
   python3 measure_slices.py        # token cost of each catalog, by slice
   python3 bench.py run --reps 3    # --only name,name  --variants pre,post
   python3 bench.py summary
   ```

The agent is `claude -p` with only the Klipp server, no built-in tools,
no user or project settings, run in an empty directory. `BENCH_MODEL`
picks the model (default `sonnet`). Runs alternate which catalog goes
first. Each starts from a new project, so runs are independent.

Results of the first comparison, before and after #669, are on #688.
