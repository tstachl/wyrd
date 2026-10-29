//! Peer credential check for the owner interface (OD-16-B caveat):
//! the uid on the other end of the socket must equal the owner's
//! uid at bind. `std` cannot do this on stable (`peer_cred` is
//! nightly-only behind `peer_credentials_unix_socket`, issue
//! #42839, still open), so this module makes the two raw syscalls
//! directly — `SO_PEERCRED` on Linux, `getpeereid` on macOS — with
//! no new dependency. One narrow `allow(unsafe_code)` covers the
//! module, following the signal-handler precedent in
//! `crate::process`: the calls take only a file descriptor and
//! out-parameters for plain integers, allocate nothing, and cannot
//! produce memory unsafety — a failed call fails the peer check
//! closed, never the process.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;

/// The uid that bound the server: this process's effective uid. The
/// owner is the process holding the store lock; only its own user
/// may consume the interface.
#[allow(unsafe_code)]
pub fn owner_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and returns the
    // caller's effective uid; it cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// The uid on the other end of `stream`, or an error. Any failure —
/// unknown platform, failed syscall, truncated credentials — is an
/// error, and the caller refuses the peer on any error.
#[allow(unsafe_code)]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `getsockopt` with `SO_PEERCRED` writes at most
        // `size_of::<ucred>()` bytes into a stack `ucred` sized
        // exactly so; `optlen` is in/out and rechecked after the
        // call. The fd is a live socket owned by `stream`, which
        // outlives the call.
        unsafe {
            let mut cred: libc::ucred = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let ret = libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut cred).cast::<libc::c_void>(),
                &mut len,
            );
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
            if len as usize != std::mem::size_of::<libc::ucred>() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "peer credentials truncated",
                ));
            }
            Ok(cred.uid)
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `getpeereid` writes two `uid_t`/`gid_t` through
        // the pointers, which name live stack slots. The fd is a
        // live socket owned by `stream`, which outlives the call.
        unsafe {
            let mut uid: libc::uid_t = 0;
            let mut gid: libc::gid_t = 0;
            let ret = libc::getpeereid(fd, &mut uid, &mut gid);
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(uid)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        compile_error!("owner interface peer authentication needs SO_PEERCRED (Linux) or getpeereid (macOS); refusing to build an unauthenticated server on this platform");
    }
}
