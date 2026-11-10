//! KLIPP PATCH: files dragged in from another client, a file manager, as
//! `WindowEvent::HoveredFile`, `HoveredFileCancelled` and `DroppedFile`, the
//! events the X11 backend gives. Upstream's Wayland backend has no
//! `wl_data_device`, so nothing dropped on a window ever arrived.
//!
//! Only `text/uri-list` is taken: `file://` URIs, one a line. The list is
//! read through the event loop, never blocking it on the other client.

use std::io::Read;
use std::path::PathBuf;

use sctk::data_device_manager::data_device::{DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::WritePipe;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::calloop::PostAction;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};

use crate::event::WindowEvent;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::WindowId;

/// The one type of a drag taken: a list of URIs.
const URI_LIST: &str = "text/uri-list";

/// The most of a list read: a drag offers paths, not files' contents.
const MAX_LIST_BYTES: usize = 1 << 20;

/// The drag over one of our windows, if any.
#[derive(Debug, Default)]
pub struct DndState {
    /// The window the drag is over.
    window: Option<WindowId>,
    /// Counts drags, so a list read for one that has gone is not used.
    drag: u64,
    /// Whether `HoveredFile` was sent for this drag.
    hovered: bool,
    /// Whether this drag was dropped.
    dropped: bool,
}

/// The paths of the `file://` URIs in a `text/uri-list` (RFC 2483): one a
/// line, CRLF or LF, `#` lines comments, percent-encoded bytes decoded.
/// Another scheme, or a file on another host, is left out.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(file_uri_path)
        .collect()
}

/// The local path a `file://` URI names.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.get(..7).filter(|s| s.eq_ignore_ascii_case("file://")).map(|_| &uri[7..])?;
    // `file:///p`, or `file://localhost/p`; another host is not here.
    let path = if rest.starts_with('/') {
        rest
    } else {
        let at = rest.find('/')?;
        if !rest[..at].eq_ignore_ascii_case("localhost") {
            return None;
        }
        &rest[at..]
    };
    let bytes = percent_decode(path)?;
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// `s` with each `%XX` its byte; `None` for a broken escape.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The drag offer on `device`, from another client.
fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

/// Read `offer`'s URI list through the event loop, then `done` with its
/// paths.
fn read_uri_list(
    state: &mut WinitState,
    offer: &DragOffer,
    done: impl FnOnce(&mut WinitState, Vec<PathBuf>) + 'static,
) {
    let pipe = match offer.receive(URI_LIST.to_owned()) {
        Ok(pipe) => pipe,
        Err(e) => {
            tracing::warn!("Drag and drop: could not receive the dropped list: {e}");
            return;
        },
    };
    let mut data = Vec::new();
    let mut done = Some(done);
    let inserted = state.loop_handle.insert_source(pipe, move |_, file, state| {
        // SAFETY: the file is read, never closed here.
        let file: &mut std::fs::File = unsafe { file.get_mut() };
        let mut buf = [0u8; 4096];
        match file.read(&mut buf) {
            Ok(0) => {
                if let Some(done) = done.take() {
                    done(state, parse_uri_list(&String::from_utf8_lossy(&data)));
                }
                PostAction::Remove
            },
            Ok(n) if data.len() + n > MAX_LIST_BYTES => {
                tracing::warn!("Drag and drop: the dropped list is over {MAX_LIST_BYTES} bytes; ignored");
                PostAction::Remove
            },
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                PostAction::Continue
            },
            Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) => {
                PostAction::Continue
            },
            Err(e) => {
                tracing::warn!("Drag and drop: reading the dropped list failed: {e}");
                PostAction::Remove
            },
        }
    });
    if let Err(e) = inserted {
        tracing::warn!("Drag and drop: could not read the dropped list: {e}");
    }
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _: f64,
        _: f64,
        surface: &WlSurface,
    ) {
        tracing::debug!(
            "Drag and drop: enter on {} (device {}), offer {}",
            surface.id(),
            device.id(),
            if drag_offer(device).is_some() { "from another client" } else { "none" }
        );
        self.dnd.drag += 1;
        self.dnd.window = Some(super::make_wid(surface));
        self.dnd.hovered = false;
        self.dnd.dropped = false;
        // Our own drags (none yet) have no offer.
        let Some(offer) = drag_offer(device) else { return };
        let takes = offer.with_mime_types(|types| {
            tracing::debug!("Drag and drop: offered {types:?}");
            types.iter().any(|t| t == URI_LIST)
        });
        if !takes {
            offer.accept_mime_type(offer.serial, None);
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.to_owned()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        let drag = self.dnd.drag;
        read_uri_list(self, &offer, move |state, paths| {
            let dnd = &mut state.dnd;
            let Some(window) = dnd.window.filter(|_| dnd.drag == drag && !dnd.dropped) else { return };
            dnd.hovered = !paths.is_empty();
            tracing::debug!("Drag and drop: hovering {} file(s)", paths.len());
            for path in paths {
                state.events_sink.push_window_event(WindowEvent::HoveredFile(path), window);
            }
        });
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        tracing::debug!("Drag and drop: leave (hovered {}, dropped {})", self.dnd.hovered, self.dnd.dropped);
        if let Some(window) = self.dnd.window.take() {
            if self.dnd.hovered && !self.dnd.dropped {
                self.events_sink.push_window_event(WindowEvent::HoveredFileCancelled, window);
            }
        }
        self.dnd.hovered = false;
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    // The clipboard is egui's own connection's; nothing of it here.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        // Logged only to show the device hears the compositor.
        tracing::debug!("Drag and drop: selection event on device {}", device.id());
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        tracing::debug!("Drag and drop: drop on device {}, over window {:?}", device.id(), self.dnd.window);
        let Some(offer) = drag_offer(device) else { return };
        let Some(window) = self.dnd.window else { return };
        if !offer.with_mime_types(|types| types.iter().any(|t| t == URI_LIST)) {
            return;
        }
        self.dnd.dropped = true;
        let finished = offer.clone();
        read_uri_list(self, &offer, move |state, paths| {
            tracing::debug!("Drag and drop: dropped {} file(s)", paths.len());
            for path in paths {
                state.events_sink.push_window_event(WindowEvent::DroppedFile(path), window);
            }
            finished.finish();
            finished.destroy();
        });
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        tracing::debug!("Drag and drop: the source offers {actions:?}");
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        action: DndAction,
    ) {
        tracing::debug!("Drag and drop: the compositor chose {action:?}");
    }
}

// Klipp offers nothing to other clients from here: no data source is made.
impl DataSourceHandler for WinitState {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

sctk::delegate_data_device!(WinitState);

#[cfg(test)]
mod tests {
    use super::parse_uri_list;
    use std::path::PathBuf;

    #[test]
    fn a_uri_list_gives_its_local_files() {
        let list = "# dragged from Files\r\n\
                    file:///home/a/My%20Film.mov\r\n\
                    file:///home/a/K%C3%B6ln%20%E6%98%A0%E7%94%BB.mp4\r\n\
                    https://example.com/clip.mp4\r\n\
                    file://localhost/tmp/x.wav\r\n\
                    file://elsewhere/tmp/y.wav\r\n\
                    \r\n\
                    FILE:///tmp/upper.mkv\n\
                    file:///tmp/broken%2\n";
        assert_eq!(
            parse_uri_list(list),
            [
                PathBuf::from("/home/a/My Film.mov"),
                PathBuf::from("/home/a/Köln 映画.mp4"),
                PathBuf::from("/tmp/x.wav"),
                PathBuf::from("/tmp/upper.mkv"),
            ]
        );
        assert!(parse_uri_list("").is_empty());
        assert!(parse_uri_list("# only a comment\n").is_empty());
    }

    #[test]
    fn a_path_that_is_not_utf8_comes_through_whole() {
        use std::os::unix::ffi::OsStrExt;
        let paths = parse_uri_list("file:///tmp/a%FFb\n");
        assert_eq!(paths[0].as_os_str().as_bytes(), b"/tmp/a\xFFb");
    }
}
