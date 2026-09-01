//! `LaunchOptions` resolution into a [`PreparedLaunch`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use camoufox_core::config::{
    get_env_vars, merge_into, set_into, spoofs_window_dimensions, validate_config,
    warn_manual_config, ConfigMap, SEED_PROPERTIES,
};
use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::fingerprint::{
    check_custom_fingerprint, determine_ua_os, from_browserforge_convert, generate_fingerprint,
    FingerprintRequest, ScreenConstraints,
};
use camoufox_core::locale::handle_locales;
use camoufox_core::mappings::warnings;
use camoufox_core::os::{host_os, SupportedOs};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use veilus_fingerprint::BrowserProfile;

/// Headless mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HeadlessMode {
    /// Headful (default).
    #[default]
    Off,
    /// Headless.
    On,
    /// Headful on a virtual display (Xvfb, Linux only).
    Virtual,
}

/// Proxy configuration (server URL with optional credentials).
///
/// # Note on authenticated proxies
///
/// Firefox ignores credentials embedded in `--proxy-server`
/// (`http://user:pass@host:port`); the `username`/`password` fields only take
/// effect when the launch is driven by an automation stack that injects
/// proxy authentication (as Playwright does). When using
/// [`crate::launch::launch`] directly, prefer a proxy without credentials or
/// one exposed on a local gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    /// Proxy server URL (e.g. `http://host:port`).
    pub server: String,
    /// Optional username.
    pub username: Option<String>,
    /// Optional password.
    pub password: Option<String>,
    /// Optional bypass list.
    pub bypass: Option<String>,
}

/// Full launch configuration.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Operating system to use for the fingerprint generation.
    /// Can be one entry or several to randomly choose from.
    /// Default: windows, macos, linux.
    pub os: Vec<SupportedOs>,

    /// Whether to block all images.
    pub block_images: bool,

    /// Whether to block WebRTC entirely.
    pub block_webrtc: bool,

    /// Whether to block WebGL. To prevent leaks, only use this for special cases.
    pub block_webgl: bool,

    /// Disables the Cross-Origin-Opener-Policy, allowing elements in
    /// cross-origin iframes to be clicked.
    pub disable_coop: bool,

    /// Calculate longitude, latitude, timezone, country and locale based on
    /// the IP address: `None` off, `Some(None)` auto-detect, `Some(Some(ip))`
    /// for a target IP.
    pub geoip: Option<Option<String>>,

    /// Humanize the cursor movement: `Some(None)` default speed,
    /// `Some(Some(seconds))` max duration.
    pub humanize: Option<Option<f64>>,

    /// Locale(s) to use. The first listed locale will be used for the Intl API.
    pub locale: Vec<String>,

    /// List of Firefox addon directories to use.
    pub addons: Vec<String>,

    /// Fonts to load into the browser (in addition to the default fonts for
    /// the target `os`).
    pub fonts: Vec<String>,

    /// If enabled, OS-specific system fonts will not be passed to the browser.
    pub custom_fonts_only: bool,

    /// Default addons to exclude.
    pub exclude_addons: Vec<camoufox_pkgman::DefaultAddon>,

    /// Constrains the screen dimensions of the generated fingerprint.
    pub screen: Option<ScreenConstraints>,

    /// Set a fixed window size instead of generating a random one.
    pub window: Option<(u32, u32)>,

    /// Use a custom fingerprint. If not provided, a random fingerprint will
    /// be generated based on the provided `os` & `screen` constraints.
    pub fingerprint: Option<BrowserProfile>,

    /// Firefox version to use. Defaults to the current Camoufox version.
    pub ff_version: Option<String>,

    /// Whether to run the browser in headless mode.
    pub headless: HeadlessMode,

    /// Whether to enable running scripts in the main world.
    pub main_world_eval: bool,

    /// Custom browser executable path.
    pub executable_path: Option<PathBuf>,

    /// Firefox user preferences to set.
    pub firefox_user_prefs: BTreeMap<String, Value>,

    /// Proxy to use for the browser. If `geoip` is auto-detect, a request
    /// will be sent through this proxy to find the target IP.
    pub proxy: Option<ProxyConfig>,

    /// Cache previous pages, requests, etc. (uses more memory).
    pub enable_cache: bool,

    /// Arguments to pass to the browser.
    pub args: Vec<String>,

    /// Extra environment variables to set (merged over the generated ones).
    pub env: BTreeMap<String, String>,

    /// Prints the config being sent to Camoufox.
    pub debug: bool,

    /// Suppresses leak warnings.
    pub i_know_what_im_doing: bool,

    /// Deterministic fingerprint seed.
    pub fingerprint_seed: Option<u64>,
}

/// Everything needed to start the browser, fully resolved.
///
/// Consumers driving Playwright themselves should treat this as the launch
/// contract: spawn `executable_path` with `env` and the `firefox_user_prefs`
/// applied to the profile, plus `args` and the proxy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedLaunch {
    /// The executable to launch.
    pub executable_path: PathBuf,
    /// Environment variables (including the `CAMOU_CONFIG_*` chunks).
    pub env: BTreeMap<String, String>,
    /// Firefox user preferences for the profile.
    pub firefox_user_prefs: BTreeMap<String, Value>,
    /// Browser arguments.
    pub args: Vec<String>,
    /// Proxy server URL with embedded credentials, if any.
    pub proxy: Option<String>,
    /// Proxy bypass list, if any.
    pub proxy_bypass: Option<String>,
    /// Whether the config spoofs window dimensions (consumers should default
    /// `viewport: null` in that case).
    pub spoofs_window_dimensions: bool,
    /// The resolved config map (debug/introspection aid).
    pub config: ConfigMap,
}

impl LaunchOptions {
    /// Validates the OS list.
    pub fn validate_os_list(&self) -> Result<()> {
        camoufox_core::os::validate_os(&self.os)?;
        Ok(())
    }
}

/// Resolves [`LaunchOptions`] into a [`PreparedLaunch`].
///
/// This performs the whole launch preparation: fingerprint generation,
/// addon/font provisioning, geoip resolution, WebGL sampling, config
/// validation and env-var chunking. Network access is required when the
/// browser or the GeoIP database must be downloaded.
pub async fn prepare(options: &LaunchOptions) -> Result<PreparedLaunch> {
    let mut config: ConfigMap = ConfigMap::new();
    let mut firefox_user_prefs = options.firefox_user_prefs.clone();

    // Warn on manual config domains (before the fingerprint merge — the
    // generated config legitimately touches these domains).
    if !options.i_know_what_im_doing {
        warn_manual_config(&config);
    }

    options.validate_os_list()?;

    // Add default addons and validate paths.
    let mut addons = options.addons.clone();
    if !addons.is_empty() || options.exclude_addons.is_empty() {
        camoufox_pkgman::add_default_addons(&mut addons, &options.exclude_addons).await?;
    }
    if !addons.is_empty() {
        camoufox_pkgman::confirm_paths(&addons)?;
        config.insert(
            "addons".into(),
            Value::Array(addons.iter().map(|a| Value::String(a.clone())).collect()),
        );
    }

    // Firefox version: installed browser's major version by default.
    let ff_version = match &options.ff_version {
        Some(version) => {
            warnings::warn_leak("ff_version", Some(options.i_know_what_im_doing));
            version.clone()
        }
        None => camoufox_pkgman::installed_ver_str()?
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string(),
    };

    // Fingerprint: generate or validate the user-supplied one.
    let fingerprint = match &options.fingerprint {
        Some(fingerprint) => {
            if !options.i_know_what_im_doing {
                check_custom_fingerprint(fingerprint)?;
            }
            fingerprint.clone()
        }
        None => generate_fingerprint(&FingerprintRequest {
            window: options.window,
            operating_systems: if options.os.is_empty() {
                None
            } else {
                Some(options.os.clone())
            },
            screen: options.screen,
            seed: options.fingerprint_seed,
        })?,
    };

    // Inject the fingerprint into the config.
    merge_into(
        &mut config,
        &from_browserforge_convert(&fingerprint, Some(&ff_version)),
    );

    // Seeds (spacing/audio/canvas): only what the installed browser declares.
    let known_properties = camoufox_core::config::load_properties(
        options.executable_path.as_deref(),
        &camoufox_pkgman::install_dir(),
    )
    .unwrap_or_default();
    for seed in SEED_PROPERTIES {
        if known_properties.contains_key(*seed) {
            set_into(
                &mut config,
                seed,
                Value::from(camoufox_core::config::random_seed() as u64),
            );
        }
    }

    // Target OS (from the UA, falling back to the host OS).
    let target_os = match config.get("navigator.userAgent").and_then(Value::as_str) {
        Some(ua) => determine_ua_os(ua)?,
        None => host_os(),
    };

    // Random window.history.length.
    set_into(
        &mut config,
        "window.history.length",
        Value::from(rand::Rng::gen_range(&mut rand::thread_rng(), 1..=5)),
    );

    // Fonts.
    if !options.fonts.is_empty() {
        config.insert(
            "fonts".into(),
            Value::Array(
                options
                    .fonts
                    .iter()
                    .map(|f| Value::String(f.clone()))
                    .collect(),
            ),
        );
    }

    if options.custom_fonts_only {
        firefox_user_prefs.insert("gfx.bundled-fonts.activate".into(), Value::from(0));
        if !options.fonts.is_empty() {
            warnings::warn_leak("custom_fonts_only", Some(options.i_know_what_im_doing));
        } else {
            return Err(CamoufoxError::Io(
                "No custom fonts were passed, but `custom_fonts_only` is enabled.".into(),
            ));
        }
    } else {
        let os_fonts = camoufox_core::mappings::fonts::fonts_for(target_os);
        let merged: Vec<Value> = {
            let mut seen = std::collections::HashSet::new();
            let mut list: Vec<Value> = Vec::new();
            for font in os_fonts
                .iter()
                .map(|f| (*f).to_string())
                .chain(options.fonts.clone())
            {
                if seen.insert(font.clone()) {
                    list.push(Value::String(font));
                }
            }
            list
        };
        config.insert("fonts".into(), Value::Array(merged));
    }

    // Proxy URL.
    let proxy_url = options.proxy.as_ref().map(proxy_to_url);

    // Geolocation via geoip.
    if let Some(geoip) = &options.geoip {
        let ip = match geoip {
            Some(ip) => ip.clone(),
            None => camoufox_geoip::public_ip(proxy_url.as_deref())
                .await
                .map_err(|e| {
                    CamoufoxError::InvalidIp(format!("failed to resolve public IP for geoip: {e}"))
                })?,
        };

        // Spoof WebRTC if not blocked.
        if !options.block_webrtc {
            if camoufox_geoip::valid_ipv4(&ip) {
                set_into(&mut config, "webrtc:ipv4", Value::String(ip.clone()));
                firefox_user_prefs.insert("network.dns.disableIPv6".into(), Value::Bool(true));
            } else if camoufox_geoip::valid_ipv6(&ip) {
                set_into(&mut config, "webrtc:ipv6", Value::String(ip.clone()));
            }
        }

        let geolocation = camoufox_geoip::get_geolocation(&ip).await?;
        for (key, value) in geolocation.as_config()? {
            config.insert(key, value);
        }
    }

    // Warn when a proxy is used without geoip.
    let uses_external_proxy = options
        .proxy
        .as_ref()
        .is_some_and(|p| !p.server.contains("localhost"));
    if uses_external_proxy && !camoufox_core::config::is_domain_set(&config, &["geolocation:"]) {
        warnings::warn_leak("proxy_without_geoip", None);
    }

    // Locale.
    if !options.locale.is_empty() {
        handle_locales(&options.locale, &mut config)?;
    }

    // Humanize.
    if let Some(max_time) = options.humanize {
        set_into(&mut config, "humanize", Value::Bool(true));
        if let Some(max_time) = max_time {
            set_into(&mut config, "humanize:maxTime", Value::from(max_time));
        }
    }

    // Main world eval.
    if options.main_world_eval {
        set_into(&mut config, "allowMainWorld", Value::Bool(true));
    }

    // Firefox prefs for feature blocks.
    if options.block_images {
        warnings::warn_leak("block_images", Some(options.i_know_what_im_doing));
        firefox_user_prefs.insert("permissions.default.image".into(), Value::from(2));
    }
    if options.block_webrtc {
        firefox_user_prefs.insert("media.peerconnection.enabled".into(), Value::Bool(false));
    }
    if options.disable_coop {
        warnings::warn_leak("disable_coop", Some(options.i_know_what_im_doing));
        firefox_user_prefs.insert(
            "browser.tabs.remote.useCrossOriginOpenerPolicy".into(),
            Value::Bool(false),
        );
    }

    // WebGL.
    if options.block_webgl {
        firefox_user_prefs.insert("webgl.disabled".into(), Value::Bool(true));
        warnings::warn_leak("block_webgl", Some(options.i_know_what_im_doing));
    } else {
        let webgl = camoufox_webgl::sample_webgl(target_os, None, None)?;
        for (key, value) in &webgl.config {
            if !config.contains_key(key) {
                config.insert(key.clone(), value.clone());
            }
        }
        firefox_user_prefs.insert(
            "webgl.enable-webgl2".into(),
            Value::Bool(webgl.webgl2_enabled),
        );
        firefox_user_prefs.insert("webgl.force-enabled".into(), Value::Bool(true));
    }

    // Canvas anti-fingerprinting.
    set_into(
        &mut config,
        "canvas:aaOffset",
        Value::from(rand::Rng::gen_range(&mut rand::thread_rng(), -50..=50)),
    );
    set_into(&mut config, "canvas:aaCapOffset", Value::Bool(true));

    // Cache prefs.
    if options.enable_cache {
        for (key, value) in camoufox_core::config::cache_prefs() {
            firefox_user_prefs.entry(key).or_insert(value);
        }
    }

    if options.debug {
        println!("[DEBUG] Config:");
        println!("{}", serde_json::to_string_pretty(&config)?);
    }

    // Validate against the browser's property schema.
    validate_config(&config, &known_properties)?;

    // Env vars: config chunks + fontconfig + caller overrides.
    let fontconfig_root = camoufox_pkgman::install_dir();
    let mut env = get_env_vars(&config, target_os, Some(&fontconfig_root))?;
    for (key, value) in &options.env {
        env.insert(key.clone(), value.clone());
    }

    // Executable path.
    let executable_path = match &options.executable_path {
        Some(path) => path.clone(),
        None => camoufox_pkgman::launch_path().await?,
    };

    Ok(PreparedLaunch {
        executable_path,
        env,
        firefox_user_prefs,
        args: options.args.clone(),
        proxy: proxy_url,
        proxy_bypass: options.proxy.as_ref().and_then(|p| p.bypass.clone()),
        spoofs_window_dimensions: spoofs_window_dimensions(&config),
        config,
    })
}

/// Renders a [`ProxyConfig`] as a URL with embedded credentials.
fn proxy_to_url(proxy: &ProxyConfig) -> String {
    let mut server = proxy.server.clone();
    if !server.contains("://") {
        server = format!("http://{server}");
    }
    match (&proxy.username, &proxy.password) {
        (Some(username), Some(password)) => {
            if let Some(position) = server.find("://") {
                let (scheme, rest) = server.split_at(position + 3);
                format!("{scheme}{username}:{password}@{rest}")
            } else {
                server
            }
        }
        (Some(username), None) => {
            if let Some(position) = server.find("://") {
                let (scheme, rest) = server.split_at(position + 3);
                format!("{scheme}{username}@{rest}")
            } else {
                server
            }
        }
        _ => server,
    }
}
