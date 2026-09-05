//! Pipe transport: spawns the browser with the Juggler pipe wired up.
//!
//! Firefox's `--juggler-pipe` mode reads commands and writes
//! responses/events over a pair of pipes, carrying NUL-delimited UTF-8 JSON
//! frames (see Playwright's `PipeTransport`).
//!
//! - **Unix**: the pipes are moved onto FDs 3/4 in a `pre_exec` hook.
//! - **Windows**: the Juggler patch reads the *inheritable OS handles* of
//!   both pipes from the `PW_PIPE_READ`/`PW_PIPE_WRITE` environment
//!   variables (see `nsRemoteDebuggingPipe.cpp` in Playwright's Firefox
//!   patch — Camoufox embeds the same patch).

#[cfg(unix)]
use std::process::Stdio;

use camoufox::builder::PreparedLaunch;

use crate::error::{JugglerError, Result};

/// The ready line Firefox prints once the pipe is listening.
pub const JUGGLER_READY_LINE: &str = "Juggler listening to the pipe";

/// A spawned browser process with the Juggler pipe wired up.
pub struct PipeTransport {
    /// The browser process.
    pub child: tokio::process::Child,
    /// Write end → browser command input.
    pub write: tokio::fs::File,
    /// Read end ← browser responses/events.
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

fn common_args(prepared: &PreparedLaunch, profile_dir: &std::path::Path, headless: bool, extra_args: &[String]) -> Vec<String> {
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

fn apply_common_env(command: &mut tokio::process::Command, prepared: &PreparedLaunch) {
    for (key, value) in &prepared.env {
        command.env(key, value);
    }
    // Remove SNAP variables that confuse Firefox (Playwright parity).
    command.env_remove("SNAP_NAME");
    command.env_remove("SNAP_INSTANCE_NAME");
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

    let mut child = command.spawn().map_err(|e| {
        JugglerError::Io(format!(
            "failed to launch {}: {e}",
            prepared.executable_path.display()
        ))
    })?;

    // Close the child-side ends in the parent (the child has its own copies).
    drop(a_read);
    drop(b_write);

    let (child, ready) = spawn_output_drain(child);
    Ok(PipeTransport {
        child,
        write: tokio::fs::File::from(std::fs::File::from(a_write)),
        read: tokio::fs::File::from(std::fs::File::from(b_read)),
        ready,
    })
}

/// Spawns the browser with the pipe handles passed via environment
/// variables, as the Windows Juggler patch expects.
#[cfg(windows)]
async fn spawn_windows(
    prepared: &PreparedLaunch,
    profile_dir: &std::path::Path,
    headless: bool,
    extra_args: &[String],
) -> Result<PipeTransport> {
    use std::os::windows::io::FromRawHandle;

    // Command pipe: browser reads ← we write. Response pipe: browser
    // writes → we read. `CreatePipe` + inheritable browser-facing ends
    // mirrors what Playwright passes as stdio[3]/stdio[4].
    let (cmd_read, cmd_write) = create_anon_pipe()?;
    let (rsp_read, rsp_write) = create_anon_pipe()?;

    let args = common_args(prepared, profile_dir, headless, extra_args);

    let mut command = tokio::process::Command::new(&prepared.executable_path);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    apply_common_env(&mut command, prepared);
    // The Juggler Windows patch turns these values into HANDLEs.
    command.env("PW_PIPE_READ", handle_to_env(cmd_read));
    command.env("PW_PIPE_WRITE", handle_to_env(rsp_write));

    let child = command.spawn().map_err(|e| {
        // The browser never started: release the browser-facing handles.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(cmd_read);
            windows_sys::Win32::Foundation::CloseHandle(rsp_write);
        }
        JugglerError::Io(format!(
            "failed to launch {}: {e}",
            prepared.executable_path.display()
        ))
    })?;

    // Wrap our ends for async IO before dropping the raw handles.
    let write = unsafe { std::fs::File::from_raw_handle(cmd_write) };
    let read = unsafe { std::fs::File::from_raw_handle(rsp_read) };
    let write = tokio::fs::File::from(write);
    let read = tokio::fs::File::from(read);

    // The browser holds its own ends; nothing to close manually (the raw
    // handle wrappers were moved into the Files above).

    let (child, ready) = spawn_output_drain(child);
    Ok(PipeTransport {
        child,
        write,
        read,
        ready,
    })
}

/// Formats a pipe handle as the env value the Juggler patch parses.
#[cfg(windows)]
fn handle_to_env(handle: windows_sys::Win32::Foundation::HANDLE) -> String {
    (handle as usize).to_string()
}

/// `CreatePipe` with an inheritable security descriptor.
#[cfg(windows)]
fn create_anon_pipe() -> Result<Handle> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    let ok = unsafe { CreatePipe(&mut read, &mut write, &security, 0) };
    if ok == 0 {
        return Err(JugglerError::Io(format!(
            "CreatePipe failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok((read, write))
}

/// Raw Win32 pipe handle pair (read, write).
#[cfg(windows)]
type Handle = (
    windows_sys::Win32::Foundation::HANDLE,
    windows_sys::Win32::Foundation::HANDLE,
);

fn spawn_output_drain(
    child: tokio::process::Child,
) -> (
    tokio::process::Child,
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
    child: &mut tokio::process::Child,
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

async fn drain_stdout(
    stdout: tokio::process::ChildStdout,
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

async fn drain_stderr(stderr: tokio::process::ChildStderr) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            log::debug!("[camoufox:err] {line}");
        }
    }
}
