//! Direct browser process launching.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use camoufox_core::error::{CamoufoxError, Result};
use serde_json::Value;

use crate::builder::{prepare, HeadlessMode, LaunchOptions, PreparedLaunch};

/// A running Camoufox browser process.
pub struct LaunchedBrowser {
    /// The underlying child process.
    pub child: tokio::process::Child,
    /// The prepared launch artifacts used to start it.
    pub prepared: PreparedLaunch,
    /// The virtual display backing the process, when one was started.
    pub virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
    /// The profile directory (temporary or persistent).
    pub profile_dir: PathBuf,
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

/// Normalizes `HeadlessMode::Virtual` into headful + Xvfb (Linux only).
///
/// Shared by the direct launcher and the Juggler driver.
pub async fn resolve_headless(
    options: &LaunchOptions,
) -> Result<(LaunchOptions, Option<camoufox_virtdisplay::VirtualDisplay>)> {
    let mut options = options.clone();
    if options.headless == HeadlessMode::Virtual {
        let mut display = camoufox_virtdisplay::VirtualDisplay::new(options.debug);
        let display_value = display.get().await?;
        options.env.insert("DISPLAY".into(), display_value);
        options.headless = HeadlessMode::Off;
        Ok((options, Some(display)))
    } else {
        Ok((options, None))
    }
}

/// Renders Firefox user prefs into `<profile_dir>/user.js`.
///
/// The browser re-reads `user.js` at every startup, so both fresh and
/// persistent profiles get the same prefs on each launch.
pub fn materialize_user_js(profile_dir: &Path, prefs: &BTreeMap<String, Value>) -> Result<()> {
    let mut contents = String::new();
    for (key, value) in prefs {
        let rendered = match value {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
            other => format!("\"{other}\""),
        };
        contents.push_str(&format!("user_pref(\"{key}\", {rendered});\n"));
    }
    std::fs::create_dir_all(profile_dir)?;
    std::fs::write(profile_dir.join("user.js"), contents)?;
    Ok(())
}

/// Resolves the profile directory for a launch.
///
/// `persistent` reuses a caller-managed directory (cookies, storage and
/// history survive across sessions); otherwise a fresh temp directory is
/// created and intentionally leaked for the browser's lifetime.
pub fn resolve_profile_dir(persistent: Option<&Path>) -> Result<PathBuf> {
    match persistent {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            Ok(dir.to_path_buf())
        }
        None => {
            let dir = tempfile::Builder::new()
                .prefix("camoufox-profile-")
                .tempdir()
                .map_err(|e| CamoufoxError::Io(e.to_string()))?;
            // Hand the directory to the browser: `keep` opts out of the
            // TempDir cleanup, keeping it alive for the browser's lifetime.
            Ok(dir.keep())
        }
    }
}

/// Prepares the launch and starts the browser process directly.
///
/// The Firefox user prefs are materialized into `user.js` inside the profile
/// directory (`--profile`), which the browser picks up on startup. When the
/// proxy carries credentials, a proxy-auth WebExtension is provisioned so
/// `--proxy-server user:pass@host:port` works without any automation driver.
pub async fn launch(options: &LaunchOptions) -> Result<LaunchedBrowser> {
    let (options, virtual_display) = resolve_headless(options).await?;

    // Authenticated proxy without a driver: provision the auth extension.
    let mut options = options;
    if let Some(proxy) = options.proxy.clone() {
        if proxy.username.is_some()
            || proxy.password.is_some()
            || crate::proxyauth::server_has_credentials(&proxy.server)
        {
            let extension = crate::proxyauth::provision(proxy)?;
            options
                .addons
                .push(extension.to_string_lossy().into_owned());
        }
    }

    let prepared = prepare(&options).await?;

    let profile_dir = resolve_profile_dir(options.persistent_profile.as_deref())?;
    materialize_user_js(&profile_dir, &prepared.firefox_user_prefs)?;

    // Build the command line.
    let mut args: Vec<String> = Vec::new();
    args.push("--profile".into());
    args.push(profile_dir.to_string_lossy().into_owned());
    if options.headless == HeadlessMode::On {
        args.push("--headless".into());
    }
    args.extend(prepared.args.iter().cloned());
    if let Some(proxy) = &prepared.proxy {
        args.push("--proxy-server".into());
        // Credentials embedded in the URL are ignored by Firefox and handled
        // by the auth extension instead.
        args.push(crate::proxyauth::strip_credentials(proxy));
    }

    let mut command = tokio::process::Command::new(&prepared.executable_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &prepared.env {
        command.env(key, value);
    }

    let child = command.spawn().map_err(|e| {
        CamoufoxError::Io(format!(
            "failed to launch {}: {e}",
            prepared.executable_path.display()
        ))
    })?;

    Ok(LaunchedBrowser {
        child,
        prepared,
        virtual_display,
        profile_dir,
    })
}
