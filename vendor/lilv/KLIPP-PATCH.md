lilv 0.2.4, patched

A verbatim copy of the crates.io `lilv` 0.2.4 sources with two changes,
both marked `KLIPP PATCH`: the UI-support callback in `src/ui.rs` takes
its URIs as `*const c_char` instead of `*const i8`, and a `#[must_use]`
rustc ignores (and warns about, now the crate is a path dependency) is
gone from `ClassIter::next` in `src/plugin.rs`.

`c_char` is `i8` on x86_64 but `u8` on aarch64 Linux, so upstream's
signature only type-checks on x86: on aarch64 the crate fails to build
and takes the LV2 host (`livi`, and with it klipp-audio) down with it.
On x86_64 the patched code is the same type as upstream's.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
It goes with the `livi` patch in `vendor/livi`. `livi` 0.9 moves to
`lilv` 0.3: moving klipp-audio to it lets this directory, `vendor/livi`
and both patch lines go.
