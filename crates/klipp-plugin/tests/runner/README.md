# Native plugin checks

From the repository root:

```sh
python3 scripts/plugin-tests.py --format ofx --scenario all --seed 932 --self-test
```

The CLI builds `klipp-plugin-test-host` offline and compiles the versioned C fixture with `cc`. Requirements: Linux x86-64, Python 3, a C11 compiler, and Rust dependencies already fetched. For gate/local reuse, pass `--host /absolute/path/to/klipp-plugin-test-host`; `cargo test -p klipp-plugin --test native_ofx` uses the binary Cargo builds for that test. No download, editor, GPU, installed plugin, or user settings are required.

Four stable `org.klipp.fixture.*` identifiers share one native `.ofx.bundle`. The worker uses production discovery, dynamic loading, initialization and `apply_effects_chain`, with explicit search directories. HOME/XDG directories are isolated per worker, and environment/system plugin paths are not searched. No raw-plugin injection is used.

| Scenario | Expected RGBA8 output | Tolerance |
|---|---|---|
| identity | Input unchanged, with a native render callback observed | Exact bytes |
| channels | `(B, 255-R, G, A)` | Exact bytes |
| alpha | `(R, G, B, floor(A/2))`, preserving RGB even at zero alpha | Exact bytes |
| time | Red increased by `floor(time*16)` modulo 256, at 0, 0.5, 1 and 3.25 seconds | Exact bytes |

Inputs combine horizontal/vertical ramps, a checker pattern, seeded noise, and alpha values 0, 1, 63, 127, 254 and 255. Python computes the reference independently; the Rust worker only loads, renders and writes actual bytes. Render markers prevent an identity test from passing when the host silently substitutes the input. These fixtures test the byte CPU path; float/GPU/temporal-neighbour capabilities remain future suites. `--backend gpu`, LV2 and CLAP return **unsupported** with a reason and exit 2.

`--self-test` additionally requires rejection of deliberately wrong output, passthrough, absent plugins, render failure (including identity), and a hung render. To inspect a failure directly, use `--scenario channels --control passthrough`. `--plugin-root` tests an existing fixture directory and never fills in missing fixtures. Missing prerequisites or required fixtures fail, rather than skip.

Each worker has a 15-second wall-clock deadline, 512 MiB address-space limit, a CPU-time limit, 8 MiB maximum output file size and core dumps disabled. Timeout kills the worker process group. `--timeout` and `--memory-mib` select bounded limits. Diagnostics are fixed at 37×23 pixels (the host rejects dimensions above 256×256). Fixture compilation has a 60-second timeout; the optional offline Rust build has a 600-second timeout. Use this worktree’s own target directory. Follow the shared memory/concurrency limits when starting builds.

Exit codes: 0 means all selected checks passed (including expected negative-control failures); 1 means failure; 2 means unsupported capability or invalid CLI arguments. Stdout and `result.json` contain schema version 1, format, backend, seed, limits, status, timings, per-frame mismatch counts and artifact paths. Raw input/reference/output RGBA files and compiler/worker logs remain under `~/.cache/klipp-tmp/plugin-tests/<run-id>`. `--output` must name a new directory outside the repository and `/tmp`. Keep these artifacts private; remove completed runs when no longer needed.

This PR supplies the OFX portion of #932. Audio fixtures and the later GPU, automation/state, crash and long-running reliability suites are not claimed by these results.
