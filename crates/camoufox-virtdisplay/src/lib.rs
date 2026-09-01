//! # camoufox-virtdisplay
//!
//! Xvfb virtual display management (Linux only).
//!
//! Xvfb is launched with `-displayfd 3` so the kernel/Xvfb itself picks a free
//! display number atomically and reports it back through file descriptor 3 —
//! no userspace race conditions. Mesa software GLX is forced via the
//! environment since the GPU is not used.
//!
//! The implementation only compiles on Linux; on other hosts the API returns
//! [`camoufox_core::error::CamoufoxError::VirtualDisplayNotSupported`].

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::time::Duration;

use camoufox_core::error::{CamoufoxError, Result};
#[cfg(target_os = "linux")]
use tokio::io::AsyncBufReadExt;

/// Timeout for Xvfb writing its display number (prevents infinite hangs).
#[cfg(target_os = "linux")]
const DISPLAYFD_READ_TIMEOUT_MS: u64 = 10_000;

/// A managed Xvfb virtual display.
pub struct VirtualDisplay {
    #[cfg(target_os = "linux")]
    debug: bool,
    #[cfg(target_os = "linux")]
    proc: Option<tokio::process::Child>,
    #[cfg(target_os = "linux")]
    display: Option<u32>,
}

#[cfg(target_os = "linux")]
impl VirtualDisplay {
    /// Creates an unstarted virtual display handle.
    pub fn new(debug: bool) -> Self {
        Self {
            debug,
            proc: None,
            display: None,
        }
    }

    /// Xvfb arguments: minimal screen, no extensions, software rendering.
    fn xvfb_args() -> Vec<String> {
        [
            "-screen",
            "0",
            "1x1x24",
            "-ac",
            "-nolisten",
            "tcp",
            "-extension",
            "RENDER",
            "+extension",
            "GLX",
            "-extension",
            "COMPOSITE",
            "-extension",
            "XVideo",
            "-extension",
            "XVideo-MotionCompensation",
            "-extension",
            "XINERAMA",
            "-fp",
            "built-ins",
            "-nocursor",
            "-br",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// Resolves the Xvfb executable path.
    fn xvfb_path(&self) -> Result<String> {
        let output = std::process::Command::new("which")
            .arg("Xvfb")
            .output()
            .map_err(|_| CamoufoxError::cannot_find_xvfb())?;
        if !output.status.success() {
            return Err(CamoufoxError::cannot_find_xvfb());
        }
        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if resolved.is_empty() {
            return Err(CamoufoxError::cannot_find_xvfb());
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&resolved)
            .map_err(|_| CamoufoxError::cannot_find_xvfb())?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(CamoufoxError::CannotExecuteXvfb(format!(
                "I do not have permission to execute Xvfb: {resolved}"
            )));
        }
        Ok(resolved)
    }

    /// Spawns Xvfb with `-displayfd 3`, waits for the display number and
    /// returns `(":<n>", child)`.
    async fn spawn_xvfb(&mut self) -> Result<String> {
        use std::os::fd::FromRawFd;
        use std::os::unix::io::AsRawFd;

        let xvfb_path = self.xvfb_path()?;

        let (read_fd, write_fd) = nix::unistd::pipe().map_err(|e| {
            CamoufoxError::CannotExecuteXvfb(format!("could not create displayfd pipe: {e}"))
        })?;

        let mut args = vec!["-displayfd".to_string(), "3".to_string()];
        args.extend(Self::xvfb_args());
        if self.debug {
            println!("Starting virtual display: {xvfb_path} {}", args.join(" "));
        }

        let mut command = tokio::process::Command::new(&xvfb_path);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(if self.debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if self.debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .env("__GLX_VENDOR_LIBRARY_NAME", "mesa")
            .env("LIBGL_ALWAYS_SOFTWARE", "1");

        let write_fd_for_exec = write_fd.as_raw_fd();
        let read_fd_for_exec = read_fd.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                // Dup the write end onto fd 3 for Xvfb to report the chosen
                // display number, and close the read end in the child.
                nix::unistd::dup2(write_fd_for_exec, 3)?;
                nix::unistd::close(read_fd_for_exec)?;
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .map_err(|e| CamoufoxError::CannotExecuteXvfb(e.to_string()))?;

        // Drop the parent's write end so EOF propagates if Xvfb dies.
        drop(write_fd);

        // Read "<display>\n" from the pipe, with a timeout.
        let read_file = unsafe { std::fs::File::from_raw_fd(read_fd.as_raw_fd()) };
        let mut reader = tokio::io::BufReader::new(tokio::fs::File::from_std(read_file));
        let mut buf = Vec::new();
        let read = tokio::time::timeout(
            Duration::from_millis(DISPLAYFD_READ_TIMEOUT_MS),
            reader.read_until(b'\n', &mut buf),
        )
        .await;

        let read = match read {
            Ok(Ok(n)) if n > 0 => n,
            Ok(Ok(_)) => {
                let _ = child.start_kill();
                return Err(CamoufoxError::CannotExecuteXvfb(
                    "Xvfb closed the displayfd pipe without reporting a display".into(),
                ));
            }
            Ok(Err(e)) => {
                let _ = child.start_kill();
                return Err(CamoufoxError::CannotExecuteXvfb(format!(
                    "failed to read displayfd: {e}"
                )));
            }
            Err(_) => {
                let _ = child.start_kill();
                return Err(CamoufoxError::CannotExecuteXvfb(format!(
                    "Xvfb did not report a display within {DISPLAYFD_READ_TIMEOUT_MS}ms"
                )));
            }
        };
        let _ = read;

        let text = String::from_utf8_lossy(&buf).trim().to_string();
        let Some(display) = text.parse::<u32>().ok() else {
            let _ = child.start_kill();
            return Err(CamoufoxError::CannotExecuteXvfb(format!(
                "Xvfb did not report a display (got {text:?})"
            )));
        };

        self.display = Some(display);
        self.proc = Some(child);
        Ok(format!(":{display}"))
    }

    /// Returns the `":<n>"` display string, starting Xvfb when needed.
    pub async fn get(&mut self) -> Result<String> {
        if self.proc.is_none() {
            self.spawn_xvfb().await
        } else {
            if self.debug {
                if let Some(display) = self.display {
                    println!("Using virtual display: {display}");
                }
            }
            match self.display {
                Some(display) => Ok(format!(":{display}")),
                None => Err(CamoufoxError::CannotExecuteXvfb(
                    "display number unavailable".into(),
                )),
            }
        }
    }

    /// Terminates Xvfb and removes its lock/socket files.
    pub fn kill(&mut self) {
        let Some(display) = self.display else {
            return;
        };
        if let Some(proc) = self.proc.as_mut() {
            let _ = proc.start_kill();
        }
        let _ = std::fs::remove_file(format!("/tmp/.X{display}-lock"));
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{display}"));
    }

    /// Waits for Xvfb to exit (best effort).
    pub async fn wait(&mut self) {
        if let Some(proc) = self.proc.as_mut() {
            let _ = tokio::time::timeout(Duration::from_secs(5), proc.wait()).await;
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xvfb_available() -> bool {
        std::process::Command::new("which")
            .arg("Xvfb")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn spawns_and_reports_display() {
        if !xvfb_available() {
            return; // Xvfb not installed on this host
        }
        let mut vd = VirtualDisplay::new(false);
        let display = vd.get().await.unwrap();
        assert!(display.starts_with(':'), "display format: {display}");
        let n: u32 = display[1..].parse().unwrap();
        assert!(std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists());

        vd.kill();
        vd.wait().await;
        assert!(!std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists());
        assert!(!std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists());
    }

    #[tokio::test]
    async fn get_is_idempotent() {
        if !xvfb_available() {
            return;
        }
        let mut vd = VirtualDisplay::new(false);
        let a = vd.get().await.unwrap();
        let b = vd.get().await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn concurrent_displays_are_unique() {
        if !xvfb_available() {
            return;
        }
        let mut vds: Vec<VirtualDisplay> = (0..10).map(|_| VirtualDisplay::new(false)).collect();
        let mut displays = Vec::new();
        for vd in &mut vds {
            displays.push(vd.get().await.unwrap());
        }
        let unique: std::collections::HashSet<_> = displays.iter().collect();
        assert_eq!(unique.len(), displays.len(), "displays must be unique");
        for vd in &mut vds {
            vd.kill();
        }
    }
}

/// Non-Linux stub: the API surface exists but always reports the platform as
/// unsupported.
#[cfg(not(target_os = "linux"))]
impl VirtualDisplay {
    /// Creates an unstarted virtual display handle.
    pub fn new(debug: bool) -> Self {
        let _ = debug;
        Self {}
    }

    /// Always fails on non-Linux platforms.
    pub async fn get(&mut self) -> Result<String> {
        Err(CamoufoxError::VirtualDisplayNotSupported(
            "Virtual display is only supported on Linux.".into(),
        ))
    }

    /// No-op on non-Linux platforms.
    pub fn kill(&mut self) {}

    /// No-op on non-Linux platforms.
    pub async fn wait(&mut self) {}
}
