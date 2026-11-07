//! Windows-only, same-user Named Pipe transport for ukk's local read-only API.
//! All Win32 calls live here. No TCP fallback and no permissive pipe DACL.
//!
//! The server creates an ACL explicitly granting full access only to the
//! current user's SID and SYSTEM. Naming by user SID is only namespacing;
//! the ACL, not the name, is the access-control boundary.
#![cfg(windows)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Windows FILE_FLAG_FIRST_PIPE_INSTANCE from winbase.h. A malicious or stale
/// server must not be silently treated as ukk's first instance.
const FIRST_INSTANCE_FLAG: u32 = 0x0008_0000;
/// SECURITY_IDENTIFICATION: the server may identify, but not impersonate,
/// the local client during pipe opening.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
const ERROR_PIPE_BUSY: i32 = 231;

fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; token is an output.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if token.is_null() {
        return Err(io::Error::new(ErrorKind::InvalidData, "OpenProcessToken returned null"));
    }
    // SAFETY: OpenProcessToken returned an owned handle. OwnedHandle closes it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut needed = 0u32;
    // SAFETY: documented size query; an insufficient-buffer return is normal.
    unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 65_536 {
        return Err(io::Error::new(ErrorKind::InvalidData, "unexpected TokenUser size"));
    }
    // TOKEN_USER contains an aligned pointer. u64-backed storage provides
    // at least 8-byte alignment on Windows targets.
    let mut aligned = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: buffer has >= needed writable bytes and remains alive.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            aligned.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful TokenUser call wrote TOKEN_USER at the aligned start.
    let user = unsafe { &*(aligned.as_ptr().cast::<TOKEN_USER>()) };
    if user.User.Sid.is_null() {
        return Err(io::Error::new(ErrorKind::InvalidData, "token user SID is null"));
    }
    let mut ptr = null_mut();
    // SAFETY: SID points into still-live TOKEN_USER storage; ptr is output.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut ptr) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if ptr.is_null() {
        return Err(io::Error::new(ErrorKind::InvalidData, "SID conversion returned null"));
    }
    // SAFETY: Windows supplied a NUL-terminated SID string. Bound scanning
    // to avoid unbounded reads in case of an inconsistent system response.
    let sid = unsafe {
        let mut len = 0usize;
        while len < 256 && *ptr.add(len) != 0 {
            len += 1;
        }
        let result = if len == 256 {
            Err(io::Error::new(ErrorKind::InvalidData, "SID text too long"))
        } else {
            String::from_utf16(std::slice::from_raw_parts(ptr, len))
                .map_err(|_| io::Error::new(ErrorKind::InvalidData, "SID not UTF-16"))
        };
        LocalFree(ptr.cast::<c_void>());
        result
    }?;
    if !sid.starts_with("S-1-") || !sid.chars().all(|c| c.is_ascii_digit() || c == 'S' || c == '-') {
        return Err(io::Error::new(ErrorKind::InvalidData, "unexpected SID text"));
    }
    Ok(sid)
}

/// Stable per-user OS pipe endpoint. No user-controllable raw pipe path
/// overrides are accepted in this initial implementation.
pub fn pipe_name() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\ukk-agent-host-{}", current_user_sid()?))
}

fn wide(input: &str) -> Vec<u16> {
    input.encode_utf16().chain(std::iter::once(0)).collect()
}

struct LocalDescriptor(*mut c_void);
impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW
            // allocated this memory, which must be released with LocalFree.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Create the first or subsequent instance of a private local Named Pipe.
/// A second Host cannot impersonate a running Host through an existing pipe.
pub fn create_server_pipe(name: &str, first_instance: bool) -> io::Result<File> {
    let sid = current_user_sid()?;
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})");
    let sddl = wide(&sddl);
    let mut descriptor = null_mut();
    // SAFETY: valid UTF-16 SDDL pointer; descriptor receives a LocalAlloc
    // buffer. It is freed by LocalDescriptor on success and error paths.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalDescriptor(descriptor);
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut flags = PIPE_ACCESS_DUPLEX;
    if first_instance {
        flags |= FIRST_INSTANCE_FLAG;
    }
    let name = wide(name);
    // SAFETY: all pointers refer to live stack/heap objects during call.
    // Handles returned on success are owned and closed by File.
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            65_536,
            65_536,
            0,
            &attrs,
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateNamedPipeW returned an owned Win32 HANDLE.
    Ok(unsafe { File::from_raw_handle(pipe) })
}

/// Blocks until a local client connects. The ERROR_PIPE_CONNECTED race
/// means a client connected between CreateNamedPipeW and this call.
pub fn accept_client(pipe: &File) -> io::Result<()> {
    // SAFETY: valid synchronous non-overlapped pipe handle; no OVERLAPPED.
    let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), null_mut()) };
    if connected != 0 {
        return Ok(());
    }
    // SAFETY: GetLastError has no preconditions.
    if unsafe { GetLastError() } == ERROR_PIPE_CONNECTED {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Connect as the current user with IDENTIFICATION-only security QoS.
/// In particular, opening a malicious named pipe may not give the remote
/// server full Windows impersonation rights over the desktop process.
pub fn open_client() -> io::Result<File> {
    let name = pipe_name()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION);
        match options.open(&name) {
            Ok(file) => return Ok(file),
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(30));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_scoped_to_real_sid() {
        let name = pipe_name().unwrap();
        assert!(name.starts_with(r"\\.\pipe\ukk-agent-host-S-1-"));
        assert!(!name.contains("..\\"));
    }

    #[test]
    fn same_user_can_connect_to_private_pipe() {
        let name = format!("{}-test-{}", pipe_name().unwrap(), std::process::id());
        let server = create_server_pipe(&name, true).unwrap();
        // A test-specific name uses the same ACL as the canonical endpoint.
        let thread = thread::spawn({
            let name = name.clone();
            move || {
                let mut opts = OpenOptions::new();
                opts.read(true).write(true).security_qos_flags(SECURITY_IDENTIFICATION);
                opts.open(name).unwrap()
            }
        });
        accept_client(&server).unwrap();
        let client = thread.join().unwrap();
        drop(client);
        drop(server);
    }
}
