livi 0.7.5, patched

A verbatim copy of the crates.io `livi` 0.7.5 sources with one change:
the C strings handed to and from LV2 are typed `c_char` instead of
`i8` (`src/features/urid_map.rs`, `src/plugin.rs`, marked
`KLIPP PATCH`).

`c_char` is `i8` on x86_64 but `u8` on aarch64 Linux, so upstream only
type-checks on x86: on aarch64 the LV2 host, and klipp-audio with it,
does not build. On x86_64 the patched code is the same type as
upstream's. It goes with the `lilv` patch in `vendor/lilv`.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
`livi` 0.9 has the fix and moves to `lilv` 0.3: moving klipp-audio to it
lets this directory, `vendor/lilv` and both patch lines go.
