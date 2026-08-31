//! Direct browser process launching.

use std::process::Stdio;

use camoufox_core::error::{CamoufoxError, Result};

use crate::builder::{prepare, HeadlessMode, LaunchOptions, PreparedLaunch};

/// A running Camoufox browser process.
pub struct LaunchedBrowser {
    /// The underlying child process.
    pub child: tokio::process::Child,
    /// The prepared launch artifacts used to start it.
    pub prepared: PreparedLaunch,
    /// The virtual display backing the process, when one was started.
    pub virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
}

impl LaunchedBrowser {
    /// The browser process id.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Waits for the browser process to exit.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child
            .wait()
            .await
            .map_err(|e| CamoufoxError::Io(e.to_string()))
    }

    /// Terminates the browser (and the virtual display, if any).
    pub async fn kill(&mut self) -> Result<()> {
        if let Some(vd) = self.virtual_display.as_mut() {
            vd.kill();
            vd.wait().await;
        }
        self.child
            .kill()
            .await
            .map_err(|e| CamoufoxError::Io(e.to_string()))?;
        Ok(())
    }
}

/// Prepares the launch and starts the browser process directly.
///
/// The Firefox user prefs are materialized into `user.js` inside the profile
/// directory (`--profile`), which the browser picks up on startup.
pub async fn launch(options: &LaunchOptions) -> Result<LaunchedBrowser> {
    let mut options = options.clone();
    let mut virtual_display = None;

    // Normalize headless: "virtual" → headful + Xvfb (Linux only).
    if options.headless == HeadlessMode::Virtual {
        let mut vd = camoufox_virtdisplay::VirtualDisplay::new(options.debug);
        let display = vd.get().await?;
        options.env.insert("DISPLAY".into(), display);
        virtual_display = Some(vd);
        options.headless = HeadlessMode::Off;
    }

    let prepared = prepare(&options).await?;

    // Materialize user.js into a fresh profile directory.
    let profile_dir = tempfile::Builder::new()
        .prefix("camoufox-profile-")
        .tempdir()
        .map_err(|e| CamoufoxError::Io(e.to_string()))?;
    let user_js = profile_dir.path().join("user.js");
    let mut contents = String::new();
    for (key, value) in &prepared.firefox_user_prefs {
        let rendered = match value {
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
            other => format!("\"{other}\""),
        };
        contents.push_str(&format!("user_pref(\"{key}\", {rendered});\n"));
    }
    std::fs::write(&user_js, contents)?;

    // Build the command line.
    let mut args: Vec<String> = Vec::new();
    args.push("--profile".into());
    args.push(profile_dir.path().to_string_lossy().into_owned());
    if options.headless == HeadlessMode::On {
        args.push("--headless".into());
    }
    args.extend(prepared.args.iter().cloned());
    if let Some(proxy) = &prepared.proxy {
        args.push("--proxy-server".into());
        args.push(proxy.clone());
    }

    // Keep the profile alive for the browser's lifetime.
    let profile_keep = profile_dir.keep();

    let mut command = tokio::process::Command::new(&prepared.executable_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &prepared.env {
        command.env(key, value);
    }

    let child = command
        .spawn()
        .map_err(|e| CamoufoxError::Io(format!(
            "failed to launch {}: {e}",
            prepared.executable_path.display()
        )))?;

    // Give the profile dir to the browser; it is intentionally leaked (the
    // browser needs it for its lifetime).
    std::mem::forget(profile_keep);

    Ok(LaunchedBrowser {
        child,
        prepared,
        virtual_display,
    })
}
