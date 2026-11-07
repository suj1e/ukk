//! Same-user verification for local Unix socket peers.
//!
//! The socket directory and file modes stop other *users* from reaching the
//! endpoint, but a mode check is only a snapshot and does not prove who is on
//! the other end of a connected stream. These OS APIs report the credentials
//! the kernel bound to the socket at connect time:
//!
//! * Linux/Android: `SO_PEERCRED` (uid/gid/pid of the connecting process).
//! * macOS and BSDs: `getpeereid` (effective uid/gid of the peer).
//!
//! Where no such check is available the Host fails closed instead of treating
//! an unverifiable peer as local. Windows uses a token SID check in
//! `ukk_host_client::windows_pipe` instead.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;

/// Reject a connected Unix peer unless the kernel reports the same effective
/// user that runs the Host.
pub(super) fn verify_same_user(stream: &UnixStream) -> Result<(), String> {
    let expected = current_uid();
    let actual = peer_uid(stream.as_raw_fd())
        .map_err(|error| format!("could not read Unix peer credentials: {error}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "rejected local client owned by uid {actual}; Host runs as uid {expected}"
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(fd: std::os::raw::c_int) -> io::Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` is a writable ucred and `length` starts at its
    // exact size; Linux writes at most `length` bytes.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected SO_PEERCRED size",
        ));
    }
    Ok(credentials.uid)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_uid(fd: std::os::raw::c_int) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both pointers refer to live stack variables.
    let result = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn peer_uid(_fd: std::os::raw::c_int) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform does not expose local socket peer credentials",
    ))
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_same_user_pair_and_reject_closed_peer() {
        let (left, right) = UnixStream::pair().unwrap();
        verify_same_user(&left).expect("same-process peer must pass");
        verify_same_user(&right).expect("same-process peer must pass");
    }

    #[test]
    fn current_uid_matches_peer_credentials() {
        let (left, _right) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(left.as_raw_fd()).unwrap(), current_uid());
    }
}
