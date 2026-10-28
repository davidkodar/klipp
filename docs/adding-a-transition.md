# Adding a new transition to Klipp

## Overview

Transition metadata (name, kind_id, default params) lives in a single registry
in `klipp-core`. CPU pixel math, GPU shaders, inspector controls, and browser
previews each live in their own file because they carry logic, not data.
Adding a new transition touches 5 files.

## Before you start

- `cargo check --workspace` and `cargo test --workspace` both pass.
- You know what the transition looks like visually, what parameters it takes,
  and what `kind_id` to assign (next available integer after the last entry in
  `TRANSITION_SPECS`).

## Checklist

### 1. Register the transition

**File:** `crates/klipp-core/src/transition_registry.rs`

Add a variant to the `TransitionKind` enum:

```rust
pub enum TransitionKind {
    // ...existing variants...
    MyNewTransition,
}
```

Add an entry to the `TRANSITION_SPECS` array:

```rust
TransitionSpec {
    kind: TransitionKind::MyNewTransition,
    kind_id: 66,                          // next available
    string_key: "MyNewTransition",        // PascalCase, matches enum variant name
    display_name: "My New Transition",    // full name for browser, combo boxes, search
    short_label: "New",                   // compact for timeline ribbon (aim for <= 8 chars)
    default_params: TransitionUserParams {
        param0: 1.0,
        param1: 0.0,
        param2: 0.0,
        param3: 0.0,
        color: [1.0, 1.0, 1.0, 1.0],
    },
},
```

Update the compile-time assertion:

```rust
const _: () = assert!(TRANSITION_SPECS.len() == 67); // was 66
```

Run `cargo check -p klipp-core` to verify.

**What each field does:**
- `kind_id` — wire ID used by the GPU shader and project files. Must be unique.
  The registry invariant tests enforce uniqueness and density.
- `string_key` — used for project save/load (JSON serialization). Must match
  the enum variant name exactly (serde derives use `Debug` format).
- `display_name` — shown in the browser card, combo boxes, search filter, and
  toast messages.
- `short_label` — shown on the timeline ribbon where space is tight.
- `default_params` — what playback uses before the user touches any inspector
  controls. Set these to sensible defaults for the transition's look.

### 2. Implement CPU pixel math

**File:** `crates/klipp-transitions/src/lib.rs`

Add a match arm in `render_transition_with_params`:

```rust
TransitionKind::MyNewTransition => {
    // Your pixel math here. Operates on &[u8] RGBA buffers.
    // Available helpers:
    //   smoothstep_f32(edge0, edge1, x) -> f32
    //   blend_pixel(from, to, t, out)
    //   apply_easing(t, curve) -> f32
    // Access user params via user_params.param0, .param1, etc.
    // Use `p` (clamped progress 0..1) for the transition timeline.
}
```

Both playback and export call this function, so it must be correct at
endpoints: `p=0` should produce pure `from`, `p=1` should produce pure `to`.

### 3. Implement the GPU shader

**File:** `crates/klipp-effects/src/gpu/shaders/transitions/transition.wgsl`

The shader is a `.wgsl` file; its `//#include` lines take in the shared
pieces under `shaders/include/`, which the crate's build step assembles
(see `build/shader_includes.rs`). The Rust side, its pipeline and
dispatch, is in `crates/klipp-effects/src/gpu/transitions.rs`.

Add an `else if` branch in the transition shader's if-chain:

```wgsl
} else if params.kind_id == 66u {
    // Your WGSL shader code here.
    // from_px and to_px are the current pixel from each clip (vec4<f32>, 0..255).
    // p is progress (0..1). User params: params.param0, params.param1, etc.
    result = ...;
}
```

**WGSL gotchas** (verified in this codebase):
- `unpack()` returns 0..255 floats, not 0..1. Normalize if doing luma math.
- No `%` operator on floats. Use `x - floor(x / m) * m` instead.
- No variable loop bounds (naga rejects them). Use fixed iteration counts.
- `smoothstep` endpoint coverage: when using `smoothstep(threshold, v)` for
  wipe-style reveals, always use `1.0 - smoothstep(...)` if revealing from
  small-v regions first. And extend the threshold range with
  `threshold = p * (max_norm + 2*softness) - softness` to guarantee full
  coverage at p=0 and p=1.
- For buffer sampling (`from_buf[idx]`, `to_buf[idx]`), clamp coordinates
  before indexing to avoid out-of-bounds.

### 4. Add inspector controls

**File:** `crates/klipp-ui/src/inspector/mod.rs`

Add a `matches!()` gated UI block inside `show_transition_inspector`:

```rust
if matches!(transition.kind, TransitionKind::MyNewTransition) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("My Param")
            .color(theme.text_secondary).size(11.0));
        let mut v = transition.user_params.param0;
        let resp = ui.add(
            egui::DragValue::new(&mut v).range(0.0..=2.0).speed(0.05),
        );
        if resp.changed() {
            let mut new_params = transition.user_params;
            new_params.param0 = v;
            cmds.push(AppCommand::SetTransitionParam {
                track_index: track_idx,
                transition_id: transition.id,
                user_params: new_params,
            });
        }
    });
}
```

If the transition has no user-configurable parameters, add a "No additional
parameters" label instead.

### 5. Add the browser entry

**File:** `crates/klipp-ui/src/browser/transitions_tab.rs`

Add a `TransitionDef` to the `TRANSITIONS` array:

```rust
TransitionDef {
    kind: klipp_core::TransitionKind::MyNewTransition,
    category: TransitionCategory::Effect,  // or Dissolve, Wipe, Slide, Zoom, Cinematic
    preview: |p, r, _| {
        // Draw a static preview into the 48px card area using painter primitives.
        // Use PREVIEW_A (blue) and PREVIEW_B (orange) for clip A/B colors.
        // Available helpers: preview_split_h, preview_split_v, preview_gradient_h,
        //                    preview_circle, preview_diagonal.
    },
},
```

The display name and kind_id are read from the registry automatically via
`spec_for_kind(def.kind)` — you don't set them here.

### 6. Verify

```bash
cargo check --workspace
cargo test --workspace
```

The registry invariant tests enforce:
- Exactly N entries in `TRANSITION_SPECS` (compile-time assertion).
- All kind_ids are unique and contiguous (0..N-1).
- All string_keys are unique.
- All display_names and short_labels are non-empty.
- `spec_for_kind`, `spec_for_kind_id`, and `spec_for_string_key` round-trip
  correctly.

If any test fails, you likely forgot to update the compile-time count
assertion or assigned a duplicate kind_id.

## What the registry does NOT own

The registry is metadata only. These files carry logic and must be updated
separately for each new transition:

| File | What it owns |
|------|-------------|
| `klipp-transitions/src/lib.rs` | CPU pixel math (the actual rendering algorithm) |
| `klipp-effects/src/gpu/shaders/transitions/transition.wgsl` | GPU compute shader (WGSL, runs on the GPU during playback); its dispatch is in `gpu/transitions.rs` |
| `klipp-ui/src/inspector/mod.rs` | Per-kind parameter controls (which sliders show for which transition) |
| `klipp-ui/src/browser/transitions_tab.rs` | Category and preview function (the static card thumbnail) |

These files do NOT need per-transition changes anymore (handled by registry):

| File | Why it's clean |
|------|---------------|
| `klipp-render/src/compositor.rs` | Calls `klipp_transitions::render_transition_with_params` directly |
| `klipp-project/src/load.rs` | Uses `spec_for_string_key` for deserialization |
| `klipp-ui/src/panels/timeline.rs` | Uses `spec_for_kind(kind).short_label` for ribbon text |
| `klipp-engine/src/transition.rs` | Uses registry for `kind_to_id`, `id_to_kind`, `all_transition_kinds` |

## Common mistakes

- **Kind_id collision** — The invariant test catches this, but double-check
  you're using the next available integer, not reusing an existing one.
- **Mismatched string_key** — Must exactly match the enum variant name
  (`"MyNewTransition"`, not `"My New Transition"`). Serde uses the variant
  name for serialization.
- **Forgetting the compile-time count** — The `const _: () = assert!(...)`
  line must be updated to the new count or the build fails.
- **GPU 0..255 vs 0..1** — The shader's `unpack()` returns pixel values in
  0..255 range. If you compute luma with `dot(px.xyz, vec3(0.2126, ...))`,
  divide by 255 first.
- **Endpoint correctness** — At `p=0` output must be pure `from`, at `p=1`
  pure `to`. Use `sin(pi*p)` envelope for effects that peak at midpoint —
  it's zero at both endpoints.
- **Missing registry entry** — The `every_transition_kind_variant_has_a_registry_entry`
  test uses an exhaustive match on `TransitionKind`. If you add a variant
  without updating the test, the build fails with a non-exhaustive pattern
  error. If you update the test without adding the registry entry,
  `spec_for_kind` panics at test time. Both paths lead you back to step 1.
