//! Path-bearing control replies are available only to the app's OS user.
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

pub(crate) fn same_user(stream: &UnixStream) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        // SAFETY: both pointers refer to live, correctly sized writable
        // objects; the descriptor belongs to this borrowed stream.
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        allowed_uid(
            (ok == 0 && len as usize == std::mem::size_of_val(&cred)).then_some(cred.uid),
            unsafe { libc::geteuid() },
        )
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: valid socket descriptor and writable uid/gid outputs.
        let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        allowed_uid((ok == 0).then_some(uid), unsafe { libc::geteuid() })
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = stream;
        false
    }
}

fn allowed_uid(peer: Option<libc::uid_t>, owner: libc::uid_t) -> bool {
    peer == Some(owner)
}

#[cfg(test)]
mod tests {
    #[test]
    fn project_access_peer_is_same_user_or_refused() {
        let (a, _) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(super::same_user(&a));
        assert!(super::allowed_uid(Some(1000), 1000));
        assert!(!super::allowed_uid(Some(1001), 1000));
        assert!(!super::allowed_uid(None, 1000));
    }
}
