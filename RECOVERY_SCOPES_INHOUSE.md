# The first in-tree scope renderer

Klipp's scopes have been drawn on the GPU in `klipp_effects::gpu_scopes`
since September 2026, after a spell on libavfilter that has since been
removed. Before either, an earlier in-tree renderer (GPU compute shaders
with a CPU fallback) drew them; it was deleted from `main` when the
libavfilter path arrived, and its full working tree is preserved at:

- **Tag**: `scopes-inhouse-final` (immutable snapshot)
- **Branch**: `legacy/scopes-inhouse` (long-lived, never deleted)
- Both point at commit `0027720`.

Nothing in the current tree depends on it. It is kept as a record of that
renderer's phosphor and splat-kernel work, not as something to bring
back: the current scopes measure at 10-bit resolution at the pane's
native size, which that renderer did not. See `docs/src/color/scopes.md`
for what ships.
