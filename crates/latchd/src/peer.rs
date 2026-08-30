//! Who is on the other end of a unix socket.
//!
//! The kernel's whole access model is "same uid as the daemon": the socket
//! file and its directory are private, and every accepted connection is
//! checked against the peer's credentials as a second, kernel-enforced line.
//! Clients check too, so a socket planted at a path a client trusts can never
//! receive keystrokes or paint bytes onto the user's terminal.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// The uid of the process at the other end of `stream`.
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = u32::MAX;
    let mut gid: libc::gid_t = u32::MAX;
    // SAFETY: getpeereid writes two ids we own.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

/// The uid of the process at the other end of `stream`.
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    // SAFETY: getsockopt writes a ucred and its length into storage we own.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERCRED returned an unexpected size",
        ));
    }
    Ok(credentials.uid)
}

/// The uid of the process at the other end of `stream`.
///
/// Platforms without a peer-credential call get no answer, which every
/// caller treats as "not us": the kernel fails closed rather than open.
#[cfg(not(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "linux"
)))]
pub fn peer_uid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer credentials are not available on this platform",
    ))
}

/// Whether the peer runs as the same user as this process.
pub fn is_same_user(stream: &UnixStream) -> bool {
    // SAFETY: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    peer_uid(stream).is_ok_and(|uid| uid == me)
}

/// Fails unless the peer runs as the same user as this process.
pub fn require_same_user(stream: &UnixStream) -> io::Result<()> {
    if is_same_user(stream) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the socket peer is not this user",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socketpair_within_one_process_is_the_same_user() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(is_same_user(&a));
        assert!(is_same_user(&b));
        require_same_user(&a).unwrap();
    }
}
