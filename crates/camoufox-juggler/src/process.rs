//! Cross-platform browser child process handle.
//!
//! On Unix this wraps [`tokio::process::Child`]. On Windows the juggler
//! pipe transport spawns the browser with a raw `CreateProcessW` (to pass
//! the MSVCRT fd-inheritance blob through `STARTUPINFO.lpReserved2`), so
//! the process is held as a raw `HANDLE` instead.

use std::io;

/// A finished process status (rendered string, e.g. `exit code: 0x…`).
pub struct ExitStatus(String);

impl std::fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stdout stream of the browser process.
#[cfg(unix)]
pub type ProcessStdout = tokio::process::ChildStdout;
/// Stdout stream of the browser process.
#[cfg(windows)]
pub type ProcessStdout = tokio::fs::File;
/// Stderr stream of the browser process.
#[cfg(unix)]
pub type ProcessStderr = tokio::process::ChildStderr;
/// Stderr stream of the browser process.
#[cfg(windows)]
pub type ProcessStderr = tokio::fs::File;

#[cfg(unix)]
struct Inner {
    child: tokio::process::Child,
}

#[cfg(windows)]
struct Inner {
    /// Raw process handle; `Send` so the async `wait` can block on it.
    process: SendHandle,
    exit_code: std::sync::Mutex<Option<u32>>,
}

#[cfg(windows)]
struct SendHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for SendHandle {}

/// A spawned browser process with the small API the driver needs:
/// [`BrowserProcess::id`], [`BrowserProcess::try_wait`],
/// [`BrowserProcess::wait`], [`BrowserProcess::kill`] plus taking the
/// stdout/stderr streams.
pub struct BrowserProcess {
    pid: u32,
    inner: Inner,
    /// The browser's stdout (readiness probe + log draining).
    pub stdout: Option<ProcessStdout>,
    /// The browser's stderr (log draining).
    pub stderr: Option<ProcessStderr>,
}

impl BrowserProcess {
    /// Wraps a `tokio::process::Child` (Unix spawn path).
    #[cfg(unix)]
    pub fn from_child(mut child: tokio::process::Child) -> Self {
        let pid = child.id().unwrap_or_default();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Self {
            pid,
            inner: Inner { child },
            stdout,
            stderr,
        }
    }

    /// Wraps a raw process handle (Windows `CreateProcessW` spawn path).
    #[cfg(windows)]
    pub fn from_raw(pid: u32, process: windows_sys::Win32::Foundation::HANDLE) -> Self {
        Self {
            pid,
            inner: Inner {
                process: SendHandle(process),
                exit_code: std::sync::Mutex::new(None),
            },
            stdout: None,
            stderr: None,
        }
    }

    /// The browser process id.
    pub fn id(&self) -> Option<u32> {
        Some(self.pid)
    }
}

#[cfg(unix)]
impl BrowserProcess {
    /// Checks whether the process exited (non-blocking).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self
            .inner
            .child
            .try_wait()?
            .map(|status| ExitStatus(status.to_string())))
    }

    /// Waits for the process to exit.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.inner.child.wait().await?;
        Ok(ExitStatus(status.to_string()))
    }

    /// Force-kills the process.
    pub async fn kill(&mut self) -> io::Result<()> {
        self.inner.child.kill().await
    }
}

#[cfg(windows)]
impl BrowserProcess {
    /// Checks whether the process exited (non-blocking).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        match unsafe { WaitForSingleObject(self.inner.process.0, 0) } {
            WAIT_OBJECT_0 => {
                let code = self.read_exit_code();
                Ok(Some(ExitStatus(format!("exit code: 0x{:X}", code))))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Waits for the process to exit.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let handle = SendHandle(self.inner.process.0);
        let waited = tokio::task::spawn_blocking(move || {
            // WAIT_FAILED is the only failure mode; the handle is valid.
            unsafe {
                windows_sys::Win32::System::Threading::WaitForSingleObject(handle.0, u32::MAX)
            }
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if waited == windows_sys::Win32::Foundation::WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let code = self.read_exit_code();
        Ok(ExitStatus(format!("exit code: 0x{:X}", code)))
    }

    /// Force-kills the process (`TerminateProcess`) and waits for it.
    pub async fn kill(&mut self) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::TerminateProcess;

        let handle = self.inner.process.0;
        unsafe {
            if TerminateProcess(handle, 1) == 0 {
                // Already dead: treat as success.
                return Ok(());
            }
        }
        let _ = self.wait().await;
        Ok(())
    }

    fn read_exit_code(&self) -> u32 {
        use windows_sys::Win32::System::Threading::GetExitCodeProcess;

        let mut cached = self.inner.exit_code.lock().unwrap();
        if let Some(code) = *cached {
            return code;
        }
        let mut code: u32 = 0;
        unsafe {
            GetExitCodeProcess(self.inner.process.0, &mut code);
        }
        *cached = Some(code);
        code
    }
}
