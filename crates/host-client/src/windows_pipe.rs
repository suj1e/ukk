//! Windows-only, same-user Named Pipe transport for ukk's local read-only API.
//! All Win32 calls live here. No TCP fallback and no permissive pipe DACL.
//!
//! Security boundary:
//! * Pipe name is derived from the current user's token SID (namespacing only).
//! * The DACL grants full access only to that SID and SYSTEM.
//! * `PIPE_REJECT_REMOTE_CLIENTS` blocks remote machines.
//! * The server verifies the connecting process token SID; the client verifies
//!   the serving process token SID. A different user is rejected in both
//!   directions even if a permissive pipe were installed.
//! * All reads and writes use overlapped I/O with a bounded deadline and
//!   `CancelIoEx`, so a dead or malicious peer cannot block a worker forever.
//!
//! Residual risk: a same-user process has the same token identity as the
//! Desktop. This transport authenticates *user*, not executable identity.
#![cfg(windows)]

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const ERROR_IO_INCOMPLETE: i32 = 996;
const INFINITE: u32 = u32::MAX;

fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; token is an output.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if token.is_null() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "OpenProcessToken returned null",
        ));
    }
    // SAFETY: OpenProcessToken returned an owned handle. OwnedHandle closes it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user_sid(token.as_raw_handle())
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: documented size query; an insufficient-buffer return is normal.
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 65_536 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "unexpected TokenUser size",
        ));
    }
    // TOKEN_USER contains an aligned pointer. u64-backed storage provides
    // at least 8-byte alignment on Windows targets.
    let mut aligned = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: buffer has >= needed writable bytes and remains alive.
    if unsafe {
        GetTokenInformation(
            token,
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
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "token user SID is null",
        ));
    }
    let mut ptr = null_mut();
    // SAFETY: SID points into still-live TOKEN_USER storage; ptr is output.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut ptr) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if ptr.is_null() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "SID conversion returned null",
        ));
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
    if !sid.starts_with("S-1-")
        || !sid
            .chars()
            .all(|c| c.is_ascii_digit() || c == 'S' || c == '-')
    {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "unexpected SID text",
        ));
    }
    Ok(sid)
}

/// SID of a process identified by Windows, used for peer verification.
fn process_user_sid(process_id: u32) -> io::Result<String> {
    if process_id == 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "named pipe peer process id is zero",
        ));
    }
    // SAFETY: OpenProcess returns an owned handle or null; pid is kernel-provided.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned an owned handle; OwnedHandle closes it.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut token = null_mut();
    // SAFETY: process handle is live; token is an output.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if token.is_null() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "OpenProcessToken returned null",
        ));
    }
    // SAFETY: OpenProcessToken returned an owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user_sid(token.as_raw_handle())
}

fn require_same_user(peer_sid: String, host_sid: &str, role: &str) -> io::Result<()> {
    if peer_sid == host_sid {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("rejected named pipe {role} SID {peer_sid}; Host user SID is {host_sid}"),
        ))
    }
}

/// Server side: prove the connected client runs as the Host user.
pub fn verify_client_same_user(pipe: &File) -> io::Result<()> {
    let mut process_id = 0u32;
    // SAFETY: connected pipe handle; process_id is an output.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut process_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let peer = process_user_sid(process_id)?;
    let host = current_user_sid()?;
    require_same_user(peer, &host, "client")
}

/// Client side: prove the connected server runs as the current user.
fn verify_server_same_user(pipe: &File) -> io::Result<()> {
    let mut process_id = 0u32;
    // SAFETY: connected pipe handle; process_id is an output.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut process_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let peer = process_user_sid(process_id)?;
    let host = current_user_sid()?;
    require_same_user(peer, &host, "server")
}

/// Stable per-user OS pipe endpoint. No user-controllable raw pipe path
/// overrides are accepted in this initial implementation.
pub fn pipe_name() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\ukk-agent-host-{}", current_user_sid()?))
}

fn wide(input: &str) -> Vec<u16> {
    input.encode_utf16().chain(std::iter::once(0)).collect()
}

fn timeout_millis(timeout: Duration) -> u32 {
    let millis = timeout.as_millis();
    if millis == 0 {
        1
    } else {
        millis.min(u32::MAX as u128) as u32
    }
}

fn timeout_error() -> io::Error {
    io::Error::new(
        ErrorKind::TimedOut,
        "named pipe operation exceeded its deadline and was cancelled",
    )
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes/name request an unnamed auto-reset=manual event.
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned an owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn complete_overlapped(handle: HANDLE, overlapped: &OVERLAPPED, wait: bool) -> io::Result<usize> {
    let mut transferred = 0u32;
    // SAFETY: `overlapped` belongs to the operation on this handle.
    let result = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, wait as i32) };
    if result != 0 {
        return Ok(transferred as usize);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == ERROR_OPERATION_ABORTED as i32 || code == ERROR_IO_INCOMPLETE => {
            Err(timeout_error())
        }
        _ => Err(error),
    }
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
///
/// The handle is opened for overlapped I/O so every read/write can carry a
/// bounded deadline.
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
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if descriptor.is_null() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Windows returned a null security descriptor",
        ));
    }
    let descriptor = LocalDescriptor(descriptor);
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first_instance {
        flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
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

/// Wait for a local client to connect using overlapped I/O. The
/// ERROR_PIPE_CONNECTED race means a client connected between
/// CreateNamedPipeW and this call.
pub fn accept_client(pipe: &File) -> io::Result<()> {
    let event = event()?;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.as_raw_handle();
    let handle = pipe.as_raw_handle();
    // SAFETY: overlapped handle requires an OVERLAPPED pointer; both stay
    // alive until the operation completes.
    let connected = unsafe { ConnectNamedPipe(handle, &mut overlapped) };
    if connected != 0 {
        return Ok(());
    }
    // SAFETY: GetLastError has no preconditions.
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    if error != ERROR_IO_PENDING {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: valid event and handle; accepting a client is allowed to wait
    // indefinitely for a local connection.
    match unsafe { WaitForSingleObject(event.as_raw_handle(), INFINITE) } {
        WAIT_OBJECT_0 => {
            complete_overlapped(handle, &overlapped, false)?;
            Ok(())
        }
        other => Err(io::Error::other(format!(
            "WaitForSingleObject returned {other} while accepting a named pipe client"
        ))),
    }
}

/// Overlapped byte-stream pipe with per-operation write/read deadlines.
pub struct PipeStream {
    file: File,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl PipeStream {
    pub fn new(file: File, read_timeout: Duration, write_timeout: Duration) -> Self {
        Self {
            file,
            read_timeout,
            write_timeout,
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.read_timeout = timeout;
    }

    /// Issue one overlapped read or write. `buffer` must be valid for
    /// `length` bytes for the duration of this call; it is only used by the
    /// kernel operation and the OVERLAPPED lives until completion/cancellation.
    fn overlapped_io(
        &self,
        buffer: *mut u8,
        length: usize,
        read: bool,
        timeout: Duration,
    ) -> io::Result<usize> {
        if length == 0 {
            return Ok(0);
        }
        if length > u32::MAX as usize {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "named pipe buffer exceeds Win32 size limit",
            ));
        }
        let event = event()?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        let handle = self.file.as_raw_handle();
        let length = length as u32;
        let mut transferred = 0u32;
        // SAFETY: the caller guarantees `buffer`/`length` validity; the
        // OVERLAPPED lives until completion or cancellation below.
        let started = unsafe {
            if read {
                ReadFile(handle, buffer, length, &mut transferred, &mut overlapped)
            } else {
                WriteFile(handle, buffer, length, &mut transferred, &mut overlapped)
            }
        };
        if started != 0 {
            return Ok(transferred as usize);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(error);
        }
        // SAFETY: valid event/handle pair for the pending operation.
        match unsafe { WaitForSingleObject(event.as_raw_handle(), timeout_millis(timeout)) } {
            WAIT_OBJECT_0 => complete_overlapped(handle, &overlapped, false),
            WAIT_TIMEOUT => {
                // Cancel the specific pending operation, then reap its result
                // so the OVERLAPPED and buffer are no longer in use.
                unsafe { CancelIoEx(handle, &overlapped) };
                complete_overlapped(handle, &overlapped, true)
            }
            other => Err(io::Error::other(format!(
                "WaitForSingleObject returned {other} during named pipe I/O"
            ))),
        }
    }
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self.read_timeout;
        let length = buffer.len();
        self.overlapped_io(buffer.as_mut_ptr(), length, true, timeout)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let timeout = self.write_timeout;
        let length = buffer.len();
        // WriteFile takes a const buffer; this cast is never written through.
        self.overlapped_io(buffer.as_ptr().cast_mut(), length, false, timeout)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Connect as the current user with IDENTIFICATION-only security QoS.
/// In particular, opening a malicious named pipe may not give the remote
/// server full Windows impersonation rights over the desktop process.
///
/// The serving process SID is verified before the client trusts the pipe.
pub fn open_client() -> io::Result<PipeStream> {
    let name = pipe_name()?;
    open_client_named(&name)
}

fn open_client_named(name: &str) -> io::Result<PipeStream> {
    let name = wide(name);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: name is a valid NUL-terminated UTF-16 string; all other
        // arguments are documented constants/null.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: CreateFileW returned an owned Win32 HANDLE.
            let file = unsafe { File::from_raw_handle(handle) };
            verify_server_same_user(&file)?;
            return Ok(PipeStream::new(
                file,
                DEFAULT_READ_TIMEOUT,
                DEFAULT_WRITE_TIMEOUT,
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(30));
            continue;
        }
        return Err(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn unique_pipe_name(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(r"\\.\pipe\ukk-test-{tag}-{}-{nanos}", std::process::id())
    }

    fn connect_without_reading(
        name: &str,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
        let (connected_tx, connected_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let name = name.to_owned();
        let handle = thread::spawn(move || {
            let stream = open_client_named(&name).expect("connect test named pipe");
            connected_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            drop(stream);
        });
        (connected_rx, release_tx, handle)
    }

    #[test]
    fn pipe_name_is_scoped_to_real_sid() {
        let name = pipe_name().unwrap();
        assert!(name.starts_with(r"\\.\pipe\ukk-agent-host-S-1-"));
        assert!(!name.contains("..\\"));
    }

    #[test]
    fn pipe_read_deadline_cancels_a_blocked_peer() {
        let name = unique_pipe_name("read-deadline");
        let server = create_server_pipe(&name, true).unwrap();
        let (connected, release, client) = connect_without_reading(&name);
        accept_client(&server).unwrap();
        connected.recv_timeout(Duration::from_secs(3)).unwrap();
        verify_client_same_user(&server).unwrap();
        let mut stream =
            PipeStream::new(server, Duration::from_millis(200), Duration::from_secs(1));
        let started = Instant::now();
        let mut byte = [0u8; 1];
        let error = stream
            .read(&mut byte)
            .expect_err("silent peer must not block");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = release.send(());
        client.join().unwrap();
    }

    #[test]
    fn pipe_write_deadline_cancels_a_blocked_peer() {
        let name = unique_pipe_name("write-deadline");
        let server = create_server_pipe(&name, true).unwrap();
        let (connected, release, client) = connect_without_reading(&name);
        accept_client(&server).unwrap();
        connected.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut stream =
            PipeStream::new(server, Duration::from_secs(1), Duration::from_millis(200));
        let payload = vec![0u8; 512 * 1024];
        let started = Instant::now();
        let error = stream
            .write_all(&payload)
            .expect_err("non-reading peer must not block");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = release.send(());
        client.join().unwrap();
    }

    #[test]
    fn client_connect_verifies_the_same_user_server() {
        // `connect_without_reading` uses `open_client_named`, which verifies
        // the serving process SID before returning the stream.
        let name = unique_pipe_name("server-sid");
        let server = create_server_pipe(&name, true).unwrap();
        let (connected, release, client) = connect_without_reading(&name);
        accept_client(&server).unwrap();
        connected.recv_timeout(Duration::from_secs(3)).unwrap();
        verify_client_same_user(&server).unwrap();
        let _ = release.send(());
        client.join().unwrap();
    }
}
