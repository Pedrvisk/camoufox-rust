//! Pipe transport: spawns the browser with the Juggler pipe wired up.
//!
//! Firefox's `--juggler-pipe` mode reads commands from fd 3 and writes
//! responses/events to fd 4, carrying NUL-delimited UTF-8 JSON frames
//! (see Playwright's `PipeTransport`).
//!
//! - **Unix**: the pipes are moved onto FDs 3/4 in a `pre_exec` hook.
//! - **Windows**: the pipes are delivered as CRT file descriptors 3/4
//!   through the MSVCRT fd-inheritance protocol — a blob in
//!   `STARTUPINFO.lpReserved2` (`int count; u8 crt_flags[count]; HANDLE
//!   os_handle[count]`, the same layout libuv writes when Node spawns
//!   Firefox). The browser's MSVCRT turns the handles into real fds,
//!   which the Juggler patch reads via `_get_osfhandle`.

#[cfg(unix)]
use std::process::Stdio;

use camoufox::builder::PreparedLaunch;

use crate::error::{JugglerError, Result};
use crate::process::BrowserProcess;

/// The ready line Firefox prints once the pipe is listening.
pub const JUGGLER_READY_LINE: &str = "Juggler listening to the pipe";

/// A spawned browser process with the Juggler pipe wired up.
pub struct PipeTransport {
    /// The browser process.
    pub child: BrowserProcess,
    /// Write end → browser command input (fd 3 on the browser side).
    pub write: tokio::fs::File,
    /// Read end ← browser responses/events (fd 4 on the browser side).
    pub read: tokio::fs::File,
    /// Set once the browser printed the ready line.
    pub ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Spawns `prepared.executable_path` with the Juggler pipe connected.
///
/// `extra_args` are appended after the standard `-juggler-pipe` argument set.
/// The proxy is intentionally NOT passed as `--proxy-server`: the driver
/// configures it (with credentials) through `Browser.setBrowserProxy`.
pub async fn spawn_with_juggler_pipe(
    prepared: &PreparedLaunch,
    profile_dir: &std::path::Path,
    headless: bool,
    extra_args: &[String],
) -> Result<PipeTransport> {
    #[cfg(unix)]
    {
        spawn_unix(prepared, profile_dir, headless, extra_args).await
    }
    #[cfg(windows)]
    {
        spawn_windows(prepared, profile_dir, headless, extra_args).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (prepared, profile_dir, headless, extra_args);
        Err(JugglerError::UnsupportedOs(
            std::env::consts::OS.to_string(),
        ))
    }
}

fn common_args(
    prepared: &PreparedLaunch,
    profile_dir: &std::path::Path,
    headless: bool,
    extra_args: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-no-remote".into(),
        "--profile".into(),
        profile_dir.to_string_lossy().into_owned(),
        "-juggler-pipe".into(),
    ];
    if headless {
        args.push("--headless".into());
    }
    args.extend(prepared.args.iter().cloned());
    args.extend(extra_args.iter().cloned());
    args.push("-silent".into());
    args
}

#[cfg(unix)]
async fn spawn_unix(
    prepared: &PreparedLaunch,
    profile_dir: &std::path::Path,
    headless: bool,
    extra_args: &[String],
) -> Result<PipeTransport> {
    use std::os::fd::AsRawFd;

    // FD 3 (child reads commands) ← pipe A write end (ours).
    // FD 4 (child writes responses) → pipe B read end (ours).
    let (a_read, a_write) = pipe_cloexec()?;
    let (b_read, b_write) = pipe_cloexec()?;

    let args = common_args(prepared, profile_dir, headless, extra_args);

    let mut command = tokio::process::Command::new(&prepared.executable_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_common_env(&mut command, prepared);

    // In the forked child: move the pipe ends onto FDs 3 and 4. dup2 clears
    // CLOEXEC on the target; the originals close at exec (FD_CLOEXEC) —
    // except when a pipe end already IS fd 3/4, where dup2 would be a no-op
    // and the flag must be cleared manually.
    let a_read_fd = a_read.as_raw_fd();
    let b_write_fd = b_write.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            move_fd(a_read_fd, 3)?;
            move_fd(b_write_fd, 4)?;
            Ok(())
        });
    }

    let child = command.spawn().map_err(|e| {
        JugglerError::Io(format!(
            "failed to launch {}: {e}",
            prepared.executable_path.display()
        ))
    })?;

    // Close the child-side ends in the parent (the child has its own copies).
    drop(a_read);
    drop(b_write);

    let child = BrowserProcess::from_child(child);
    let (child, ready) = spawn_output_drain(child);
    Ok(PipeTransport {
        child,
        write: tokio::fs::File::from(std::fs::File::from(a_write)),
        read: tokio::fs::File::from(std::fs::File::from(b_read)),
        ready,
    })
}

/// Spawns the browser on Windows, delivering the pipe pair as CRT fds 3/4
/// through `STARTUPINFO.lpReserved2` (the mechanism Node/libuv uses when
/// Playwright launches Firefox — see libuv's `src/win/process-stdio.c`).
#[cfg(windows)]
async fn spawn_windows(
    prepared: &PreparedLaunch,
    profile_dir: &std::path::Path,
    headless: bool,
    extra_args: &[String],
) -> Result<PipeTransport> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, HANDLE, TRUE};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, SetHandleInformation, FILE_SHARE_READ, FILE_SHARE_WRITE, HANDLE_FLAG_INHERIT,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOW,
    };

    // MSVCRT fd flags (see libuv's process-stdio.c).
    const FOPEN: u8 = 0x01;
    const FPIPE: u8 = 0x08;
    const FDEV: u8 = 0x40;

    let inheritable = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };

    let create_pipe = || -> Result<(HANDLE, HANDLE)> {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &inheritable, 0) } == 0 {
            return Err(JugglerError::Io(format!(
                "CreatePipe failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok((read, write))
    };

    // stdout / stderr: parent reads, browser writes.
    let (stdout_read, stdout_write) = create_pipe()?;
    let (stderr_read, stderr_write) = create_pipe()?;
    // Commands: parent writes (cmd_write), browser reads fd 3 (cmd_read).
    let (cmd_read, cmd_write) = create_pipe()?;
    // Responses: browser writes fd 4 (rsp_write), parent reads (rsp_read).
    let (rsp_read, rsp_write) = create_pipe()?;

    // The parent-side ends must not leak into the browser: mark them
    // non-inheritable so EOF semantics work when either side exits.
    let no_inherit = |handle: HANDLE| {
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    };
    no_inherit(stdout_read);
    no_inherit(stderr_read);
    no_inherit(cmd_write);
    no_inherit(rsp_read);

    // fd 0: the NUL device (Firefox expects a readable stdin).
    let nul_path: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
    let nul = unsafe {
        CreateFileW(
            nul_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &inheritable,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if nul.is_null() {
        return Err(JugglerError::Io(format!(
            "CreateFile(NUL) failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    // The child stdio blob the MSVCRT parses at startup:
    //   int count; u8 crt_flags[count]; HANDLE os_handle[count]
    let handles = [nul, stdout_write, stderr_write, cmd_read, rsp_write];
    let flags = [FOPEN | FDEV, FOPEN | FPIPE, FOPEN | FPIPE, FOPEN | FPIPE, FOPEN | FPIPE];
    let mut blob: Vec<u8> = Vec::with_capacity(4 + flags.len() + handles.len() * 8);
    blob.extend_from_slice(&(handles.len() as u32).to_le_bytes());
    blob.extend_from_slice(&flags);
    for handle in handles {
        blob.extend_from_slice(&(handle as usize).to_le_bytes());
    }

    // Command line: application + quoted arguments.
    let args = common_args(prepared, profile_dir, headless, extra_args);
    let mut cmdline = quote_windows_arg(&prepared.executable_path.to_string_lossy());
    for arg in &args {
        cmdline.push(' ');
        cmdline.push_str(&quote_windows_arg(arg));
    }
    let mut cmdline_utf16: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    let application_utf16: Vec<u16> = prepared
        .executable_path
        .as_os_str()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Environment block (current env + launch overrides, SNAP vars removed).
    let environment = build_environment_block(prepared);

    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = nul;
    startup.hStdOutput = stdout_write;
    startup.hStdError = stderr_write;
    startup.cbReserved2 = blob.len() as u16;
    startup.lpReserved2 = blob.as_mut_ptr();

    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            application_utf16.as_ptr(),
            cmdline_utf16.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            TRUE, // bInheritHandles: the CRT handles reach the child.
            CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr() as *const _,
            std::ptr::null(),
            &startup,
            &mut info,
        )
    };
    if created == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            for handle in handles {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            for handle in [stdout_read, stderr_read, cmd_write, rsp_read] {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
        }
        return Err(JugglerError::Io(format!(
            "failed to launch {}: {error}",
            prepared.executable_path.display()
        )));
    }

    unsafe {
        // The main thread handle is not needed.
        windows_sys::Win32::Foundation::CloseHandle(info.hThread);
        // The browser owns its own copies now; drop ours so pipe EOF
        // propagates when either side exits.
        for handle in handles {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
    }

    let file = |handle: HANDLE| {
        let file = std::fs::File::from_raw_handle(handle);
        tokio::fs::File::from(file)
    };

    let mut child = BrowserProcess::from_raw(info.dwProcessId, info.hProcess);
    child.stdout = Some(file(stdout_read));
    child.stderr = Some(file(stderr_read));
    let (child, ready) = spawn_output_drain(child);

    Ok(PipeTransport {
        child,
        write: file(cmd_write),
        read: file(rsp_read),
        ready,
    })
}

/// Builds the UTF-16 environment block for `CreateProcessW` (the current
/// environment plus the launch overrides, with Firefox-hostile SNAP
/// variables removed).
#[cfg(windows)]
fn build_environment_block(prepared: &PreparedLaunch) -> Vec<u16> {
    let mut vars: std::collections::BTreeMap<String, String> = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    vars.remove("SNAP_NAME");
    vars.remove("SNAP_INSTANCE_NAME");
    for (key, value) in &prepared.env {
        vars.insert(key.clone(), value.clone());
    }
    let mut block: Vec<u16> = Vec::new();
    for (key, value) in &vars {
        block.extend(key.encode_utf16());
        block.push('=' as u16);
        block.extend(value.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Quotes one command-line argument following the MS `argv` parsing rules.
#[cfg(windows)]
fn quote_windows_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".into();
    }
    if !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                backslashes = 0;
                out.push('"');
            }
            other => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(other);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(unix)]
fn apply_common_env(command: &mut tokio::process::Command, prepared: &PreparedLaunch) {
    for (key, value) in &prepared.env {
        command.env(key, value);
    }
    // Remove SNAP variables that confuse Firefox (Playwright parity).
    command.env_remove("SNAP_NAME");
    command.env_remove("SNAP_INSTANCE_NAME");
}

fn spawn_output_drain(
    child: BrowserProcess,
) -> (
    BrowserProcess,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut child = child;
    // Take over stdout: readiness detection + log draining.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(drain_stdout(stdout, ready.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(drain_stderr(stderr));
    }
    (child, ready)
}

/// Creates a pipe with `FD_CLOEXEC` set on both ends.
///
/// `pipe2(O_CLOEXEC)` is not portable (`nix` gates it to a subset of
/// Unixes — notably absent on macOS), so the flag is applied with `fcntl`
/// after a plain `pipe(2)`.
#[cfg(unix)]
fn pipe_cloexec() -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::AsRawFd;

    let (read, write) = nix::unistd::pipe().map_err(|e| JugglerError::Io(format!("pipe: {e}")))?;
    for fd in [read.as_raw_fd(), write.as_raw_fd()] {
        nix::fcntl::fcntl(
            fd,
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
        )
        .map_err(|e| JugglerError::Io(format!("fcntl FD_CLOEXEC: {e}")))?;
    }
    Ok((read, write))
}

/// Moves `from` onto `to` in the child's fd table (async-signal-safe).
#[cfg(unix)]
fn move_fd(from: std::os::raw::c_int, to: std::os::raw::c_int) -> std::io::Result<()> {
    use std::io::Error;
    if from == to {
        // dup2 would be a no-op and leave CLOEXEC set; clear it.
        let flags = nix::fcntl::FdFlag::empty();
        if nix::fcntl::fcntl(to, nix::fcntl::FcntlArg::F_SETFD(flags)).is_err() {
            return Err(Error::last_os_error());
        }
        return Ok(());
    }
    nix::unistd::dup2(from, to)
        .map(|_| ())
        .map_err(|_| Error::last_os_error())
}

/// Waits for the `Juggler listening to the pipe` line (readiness probe).
pub async fn wait_ready(
    child: &mut BrowserProcess,
    ready: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if ready.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(JugglerError::Io(format!(
                "browser exited before the Juggler pipe was ready (status {status})"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(JugglerError::Timeout(
                "waiting for 'Juggler listening to the pipe'".into(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn drain_stdout<R: tokio::io::AsyncRead + Unpin>(
    stdout: R,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains(JUGGLER_READY_LINE) {
            ready.store(true, std::sync::atomic::Ordering::Release);
        } else if !line.trim().is_empty() {
            log::debug!("[camoufox] {line}");
        }
    }
}

async fn drain_stderr<R: tokio::io::AsyncRead + Unpin>(stderr: R) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            log::debug!("[camoufox:err] {line}");
        }
    }
}
