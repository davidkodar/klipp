# winit 0.30.13, patched

A verbatim copy of the crates.io `winit` 0.30.13 sources with one change
in the Wayland backend: a redraw request is held back by a pending
frame callback for at most 100 ms (`window/state.rs`,
`event_loop/mod.rs`, marked `KLIPP PATCH`).

Upstream refuses every redraw while a frame callback is pending, and a
compositor sends that callback only when it renders. Hyprland on an
idle desktop with a hardware cursor renders about once a second (the
bar's clock), so Klipp painted once a second under hundreds of input
events per second and looked hung. The patch lets the redraw go ahead
after 100 ms; its commit carries damage, the compositor renders, the
callback arrives, and the normal cadence resumes.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Drop this directory and the patch line when the workspace moves to a
winit release that bounds the wait itself.

## Files dropped from other clients (Wayland)

Upstream's Wayland backend has no `wl_data_device`, so a file dragged
from a file manager onto a window sent nothing: `HoveredFile`,
`HoveredFileCancelled` and `DroppedFile` came only on X11, Windows and
macOS. `wayland/dnd.rs` (new) binds the data device manager, makes a data
device on each seat (`state.rs`, `seat/mod.rs`), and on a drag over one of
our windows accepts `text/uri-list` as a copy, reads it through the event
loop, and sends `HoveredFile` for each `file://` path, then
`HoveredFileCancelled` if the drag leaves or `DroppedFile` for each path
on the drop. Other types (a Flatpak's `application/vnd.portal.filetransfer`
alone, or text) are refused; the types offered are logged at debug level.
The selection (clipboard) is not touched: egui's clipboard has its own
connection. All marked `KLIPP PATCH`.

The URI list's parser has tests in `dnd.rs`. The crate is not a workspace
member, so they run on a copy outside the workspace:
`cp -r vendor/winit /tmp/w && cd /tmp/w && cargo test --lib dnd`.
Drop this with the rest of the directory when winit's own Wayland backend
takes drops.
