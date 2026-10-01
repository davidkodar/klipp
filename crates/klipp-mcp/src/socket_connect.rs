//! A bounded Unix socket connect. A full server backlog can otherwise leave
//! the MCP worker blocked before stream read/write timeouts are installed.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub(super) fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "control socket path is too long"));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) { *target = *source as libc::c_char; }
    let length = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 { return Err(io::Error::last_os_error()); }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let result = unsafe { libc::connect(fd.as_raw_fd(), &address as *const _ as *const libc::sockaddr, length) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EAGAIN)) { return Err(error); }
        let mut pollfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut pollfd, 1, ms) };
        if ready == 0 { return Err(io::Error::new(io::ErrorKind::TimedOut, "control socket connect timed out")); }
        if ready < 0 { return Err(io::Error::last_os_error()); }
        let mut socket_error: libc::c_int = 0;
        let mut error_len = std::mem::size_of_val(&socket_error) as libc::socklen_t;
        let checked = unsafe { libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_ERROR,
            &mut socket_error as *mut _ as *mut libc::c_void, &mut error_len) };
        if checked < 0 { return Err(io::Error::last_os_error()); }
        if socket_error != 0 { return Err(io::Error::from_raw_os_error(socket_error)); }
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
