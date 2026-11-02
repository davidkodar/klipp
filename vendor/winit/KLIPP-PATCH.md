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

A held redraw also bounds the event loop's sleep (`held_redraw_timeout`
in `event_loop/mod.rs`), so it goes ahead at 100 ms even with no event
to wake the loop. Without that, a window a compositor sends no callbacks
for, such as a fullscreen one on a headless output, drew only when some
other event arrived. The bound applies only while a redraw is waiting:
a window with nothing to draw still sleeps until an event comes.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Drop this directory and the patch line when the workspace moves to a
winit release that bounds the wait itself.
