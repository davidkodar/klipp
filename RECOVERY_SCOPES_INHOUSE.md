# Recovering the in-tree scope renderer

The legacy in-tree GPU compute shaders + CPU fallback that powered
Klipp's scopes (waveform / parade / vectorscope / histogram) before
the migration to libavfilter were removed from `main`. The full
working tree as it existed immediately before the deletion is
preserved at:

- **Tag**: `scopes-inhouse-final` (immutable snapshot)
- **Branch**: `legacy/scopes-inhouse` (long-lived, never deleted)
- Both point at commit `0027720`.

If you need to bring the in-tree path back, the choice is between a
**file-level cherry-pick** (drop the legacy files back into the
current tree) or a **full branch checkout** (build the entire
legacy code base).

## When you'd want to recover

- A bug surfaces in `libavfilter`'s waveform / vectorscope / histogram
  filters that's unfixable upstream in a reasonable timeframe.
- A platform target appears where `ffmpeg-next` won't build but
  `wgpu` does.
- Performance regression in the FFmpeg path under specific content.

## Step 1 — Skim the legacy code

```bash
git fetch origin scopes-inhouse-final
git checkout scopes-inhouse-final -- crates/klipp-effects/src/gpu.rs \
    crates/klipp-ui/src/widgets/scopes.rs \
    crates/klipp-engine/src/engine.rs
```

Read what's there. The relevant entry points were:

- `klipp_effects::gpu::GpuEffectsPipeline::render_scope_waveform`
  / `render_scope_parade` / `render_scope_histogram`
- `klipp_ui::widgets::scopes::render_waveform_*` /
  `render_parade_direct` / `render_histogram_*` / `render_vectorscope`
- `klipp_engine::engine`'s four scope dispatch sites (search for
  `gpu_scope_kind_for_mode`).

## Step 2 — Decide on the recovery shape

**Partial revert** (bring in-tree back alongside FFmpeg, env-gated):
1. Re-introduce the `KLIPP_SCOPE_FFMPEG` env check at the four
   dispatch sites (default whichever path you want).
2. Restore the legacy `gpu_scope_kind_for_mode` + the per-mode
   `render_scope_*` methods on `GpuEffectsPipeline`.
3. Restore the CPU `render_*` functions in `widgets/scopes.rs` and
   the `ScopeAction::CpuRender` fallback in `show_scopes`.

**Full revert** (drop FFmpeg path entirely): just
`git checkout scopes-inhouse-final` and start a new feature branch
off it. You'll lose every FFmpeg-only feature shipped after the tag
(per-mode filter / envelope / intensity / vectorscope graticule
options / histogram colour mode / luma toggle / etc.).

## Step 3 — Expect compile-rot

The legacy code referenced engine types, frame-buffer fields, and
state structs as they existed at `0027720`. Any engine API change
since (LinearFrame layout, FrameBuffer fields, AppCommand variants,
etc.) needs reconciling by hand. Treat the recovery as a meaningful
porting exercise, not a one-click revert.

## Step 4 — Tests

A revert should re-introduce the legacy parity tests under
`crates/klipp-ui/src/widgets/scopes.rs` (search for `mod tests {`
inside the file at the tag). The new
`crates/klipp-engine/tests/ffmpeg_scope_smoke.rs` tests should keep
passing — they cover the FFmpeg path which a partial revert leaves
in place.

---

Filed during Phase 8 migration close. The tag and branch are
infrastructure, not personal preferences — please don't garbage-
collect them without a project-level decision to drop legacy
support entirely.
