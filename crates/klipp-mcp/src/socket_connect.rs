//! Bound the time spent opening a control socket without adding unsafe code.
//! A full local listen backlog can otherwise hold the MCP worker before
//! stream read and write timeouts have been installed.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub(super) fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let path = path.to_owned();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new().name("klipp-socket-connect".into()).spawn(move || {
        let _ = sender.send(UnixStream::connect(path));
    })?;
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut, "control socket connect timed out")),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
            io::ErrorKind::BrokenPipe, "control socket connect worker stopped")),
    }
}
