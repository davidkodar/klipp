# smithay-clipboard 0.7.3, patched

A verbatim copy of the crates.io `smithay-clipboard` 0.7.3 sources (MIT,
`LICENSE` kept), with one change: its data device also takes files dropped
on the app's window from another client, a file manager (`src/dnd.rs`,
new; `src/state.rs` and `src/lib.rs`, marked `KLIPP PATCH`).

winit's Wayland backend has no data device, so a file dragged onto the
window sent nothing; only X11 has drops. A data device made for it is not
enough either: Mutter keeps one data device per client and seat, the
newest, and sends that client's drags and its clipboard to it alone. egui's
clipboard (this crate, on the same `wl_display` through
`Backend::from_foreign_display`) makes its device after the window, so on
GNOME a winit device got nothing, and making one later would take the
clipboard's selection away. So this one device serves both.

On a drag over the window it accepts `text/uri-list` as a copy (other
types are refused); on the drop it reads the list through the clipboard
worker's own loop, queues the local paths of its `file://` URIs
(percent-decoded, comments, other schemes and other hosts left out), and
calls the waker the app set. The app takes them with
`take_dropped_files()` and shows a drag over the window with
`drag_hovering()` (`klipp-ui`, `app/frame_loop.rs`). The selection,
primary selection, copy and paste code is unchanged.

The parser and the queue have tests in `src/dnd.rs`. The crate is not a
workspace member, so they run on a copy outside the workspace:
`cp -r vendor/smithay-clipboard /tmp/c && cd /tmp/c && cargo test --lib`.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Drop
this directory and the patch line when winit or egui take Wayland drops
themselves.
