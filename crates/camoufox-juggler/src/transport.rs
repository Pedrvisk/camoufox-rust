//! Pipe transport: spawns the browser with FDs 3/4 wired to Juggler.
//!
//! Firefox's `--juggler-pipe` mode reads commands from FD 3 and writes
//! responses/events to FD 4. Messages are UTF-8 JSON frames delimited by a
//! single NUL byte (see Playwright's `PipeTransport`).

use std::process::Stdio;

use camoufox::builder::PreparedLaunch;
use tokio::io::AsyncWriteExt;

use crate::error::{JugglerError, Result};

/// The ready line Firefox prints once the pipe is listening.
pub const JUGGLER_READY_LINE: &str = "Juggler listening to the pipe";

/// A spawned browser process with the Juggler pipe wired up.
pub struct PipeTransport {
    /// The browser process.
    pub child: tokio::process::Child,
    /// Write end → child FD 3 (commands).
    pub write: tokio::fs::File,
    /// Read end ← child FD 4 (responses/events).
    pub read: tokio::fs::File,
    /// Set once the browser printed the ready line.
    pub ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Spawns `prepared.executable_path` with the Juggler pipe on FDs 3/4.
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
    #[cfg(not(unix))]
    {
        let _ = (prepared, profile_dir, headless, extra_args);
        Err(JugglerError::UnsupportedOs(
            std::env::consts::OS.to_string(),
        ))
    }
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
    let flags = nix::fcntl::OFlag::O_CLOEXEC;
    let (a_read, a_write) =
        nix::unistd::pipe2(flags).map_err(|e| JugglerError::Io(format!("pipe: {e}")))?;
    let (b_read, b_write) =
        nix::unistd::pipe2(flags).map_err(|e| JugglerError::Io(format!("pipe: {e}")))?;

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

    let mut command = tokio::process::Command::new(&prepared.executable_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &prepared.env {
        command.env(key, value);
    }
    // Remove SNAP variables that confuse Firefox (Playwright parity).
    command.env_remove("SNAP_NAME");
    command.env_remove("SNAP_INSTANCE_NAME");

    // In the forked child: move the pipe ends onto FDs 3 and 4. dup2 clears
    // CLOEXEC on the target; the originals close at exec (O_CLOEXEC) —
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

    // Take over stdout: readiness detection + log draining.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(drain_stdout(stdout, ready.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(drain_stderr(stderr));
    }

    Ok(PipeTransport {
        child,
        write: tokio::fs::File::from(std::fs::File::from(a_write)),
        read: tokio::fs::File::from(std::fs::File::from(b_read)),
        ready,
    })
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

/// Writes one NUL-terminated JSON frame.
pub async fn write_frame(writer: &mut tokio::fs::File, message: &serde_json::Value) -> Result<()> {
    let mut frame = serde_json::to_string(message)?.into_bytes();
    frame.push(b'\0');
    writer
        .write_all(&frame)
        .await
        .map_err(|e| JugglerError::Io(format!("pipe write: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| JugglerError::Io(format!("pipe flush: {e}")))?;
    Ok(())
}
