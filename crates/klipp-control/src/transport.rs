//! Holding the transport still while a read looks at a frame, and putting
//! it back as it was (#826).

#[allow(unused_imports)]
use super::*;

/// `text` within `max_chars`, cut on a line boundary and saying so, and
/// whether it was cut.
/// Where the user's viewer was before a frame read moved it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HeldTransport {
    pub(crate) at: Option<klipp_core::Timecode>,
    pub(crate) playing: bool,
}

/// Note where the user is before a read moves the playhead. The UI's
/// own playhead, as it publishes it, not the last frame the viewer
/// presented: that lags a seek, and in a window that is not being drawn
/// it can be minutes stale, so a read "put back" to it left the
/// playhead, and the Colour page following it, somewhere else.
pub(crate) fn hold_transport(ctx: &ControlContext) -> Result<HeldTransport, String> {
    let published = ctx.shared.transport.lock().map(|t| *t).unwrap_or_default();
    let presented = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.shown_presented();
    Ok(held_from(published, presented))
}

pub(crate) fn held_from(published: klipp_core::TransportStatus, presented: Option<klipp_core::Timecode>) -> HeldTransport {
    HeldTransport { at: published.playhead.or(presented), playing: published.playing }
}

/// What puts the viewer back after a read: the playhead where it was,
/// and playback running again if it was running.
pub(crate) fn restore_commands(held: HeldTransport) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    if let Some(t) = held.at {
        cmds.push(AppCommand::SeekTo(t));
    }
    if held.playing {
        cmds.push(AppCommand::Play);
    }
    cmds
}

pub(crate) fn restore_transport(ctx: &ControlContext, held: HeldTransport) {
    let cmds = restore_commands(held);
    if !cmds.is_empty() {
        let _ = send_envelope(ctx, cmds, false);
    }
}
