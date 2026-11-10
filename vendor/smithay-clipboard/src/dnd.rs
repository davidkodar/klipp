//! KLIPP PATCH: files dropped on the app's window from another client, a
//! file manager. A compositor may send a client's drags to one data device
//! only: Mutter keeps the newest a client made per seat, and that is this
//! crate's, made after the window's. So the clipboard's device takes the
//! drops too, and the app collects them here.
//!
//! Only `text/uri-list` is taken: `file://` URIs, one a line.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The one type of a drag taken: a list of URIs.
pub(crate) const URI_LIST: &str = "text/uri-list";

/// The most of a list read: a drag offers paths, not files' contents.
pub(crate) const MAX_LIST_BYTES: usize = 1 << 20;

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Drops {
    hovering: bool,
    dropped: Vec<PathBuf>,
    waker: Option<Waker>,
}

static DROPS: Mutex<Drops> = Mutex::new(Drops { hovering: false, dropped: Vec::new(), waker: None });

fn drops() -> std::sync::MutexGuard<'static, Drops> {
    DROPS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Call `wake` when a drag enters or leaves the window or files are
/// dropped, so the app looks without waiting for other input: during a
/// drag the window gets no pointer events.
pub fn set_drop_waker(wake: impl Fn() + Send + Sync + 'static) {
    drops().waker = Some(Arc::new(wake));
}

/// The files dropped since the last call.
pub fn take_dropped_files() -> Vec<PathBuf> {
    std::mem::take(&mut drops().dropped)
}

/// Whether a drag of files is over the window now.
pub fn drag_hovering() -> bool {
    drops().hovering
}

fn wake() {
    let waker = drops().waker.clone();
    if let Some(wake) = waker {
        wake();
    }
}

pub(crate) fn set_hovering(hovering: bool) {
    let changed = std::mem::replace(&mut drops().hovering, hovering) != hovering;
    if changed {
        wake();
    }
}

pub(crate) fn push_dropped(paths: Vec<PathBuf>) {
    {
        let mut drops = drops();
        drops.hovering = false;
        drops.dropped.extend(paths);
    }
    wake();
}

/// The paths of the `file://` URIs in a `text/uri-list` (RFC 2483): one a
/// line, CRLF or LF, `#` lines comments, percent-encoded bytes decoded.
/// Another scheme, or a file on another host, is left out.
pub(crate) fn parse_uri_list(text: &str) -> Vec<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The app is woken on a drop and on a drag's coming and going, and
    /// takes each drop once.
    #[test]
    fn a_drop_wakes_the_app_and_is_taken_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        set_drop_waker(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        set_hovering(true);
        assert!(drag_hovering());
        set_hovering(true);
        assert_eq!(woken.load(Ordering::SeqCst), 1, "only a change wakes");
        push_dropped(parse_uri_list("file:///tmp/a.mov\r\n"));
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert!(!drag_hovering(), "a drop ends the hover");
        assert_eq!(take_dropped_files(), [PathBuf::from("/tmp/a.mov")]);
        assert!(take_dropped_files().is_empty(), "taken once");
    }
}
