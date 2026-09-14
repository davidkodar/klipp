# egui-wgpu 0.29.1, patched

A verbatim copy of the crates.io `egui-wgpu` 0.29.1 sources, with one
change in `src/winit.rs`, `paint_and_update_textures`: textures freed
during the frame are destroyed after the frame's `Queue::submit`, not
before it.

Upstream 0.29.1 destroys them first. wgpu refuses a submit whose command
buffer references a destroyed texture, so any texture freed in the same
frame it was painted — a `TextureHandle` dropped after its image was
drawn, an image loader evicting an entry, a handle re-uploaded at a new
size — aborted the app:

    Error in Queue::submit: Validation Error
    Caused by: Texture with 'egui_texid_Managed(1268)' label has been destroyed

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Drop this directory and the patch line when the workspace moves to an
egui release that frees after submit.
