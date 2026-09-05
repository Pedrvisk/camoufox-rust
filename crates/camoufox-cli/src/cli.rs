//! Command-line interface: fetch, remove, path, version, prepare, test,
//! launch, verify, persona.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use camoufox::builder::{prepare, HeadlessMode, LaunchOptions, ProxyConfig};
use camoufox::launch::launch;
use camoufox_core::error::Result;
use camoufox_core::os::SupportedOs;
use camoufox_core::persona::{PersonaCookie, PersonaLocalStorage, SessionSnapshot};
use camoufox_store::{open as open_store, PersonaStore, SessionPersistence};

#[derive(Parser)]
#[command(
    name = "camoufox",
    about = "Manage and launch the Camoufox anti-detect browser",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download/update the Camoufox binaries, GeoIP database and addons.
    Fetch,
    /// Remove the Camoufox binaries and GeoIP database.
    Remove,
    /// Print the install directory.
    Path,
    /// Print the installed binary version.
    Version,
    /// Resolve launch options and print the prepared launch as JSON.
    Prepare {
        /// Constrain the fingerprint OS.
        #[arg(long)]
        os: Option<String>,
        /// Run headless during preparation (affects screen constraints only).
        #[arg(long)]
        headless: bool,
        /// Include the resolved config map in the output.
        #[arg(long)]
        with_config: bool,
        /// Use a stored persona (see `camoufox persona`).
        #[arg(long)]
        persona: Option<String>,
        /// Persona store spec (file[:<dir>], sqlite[:<path>], mysql:<dsn>, s3://…).
        #[arg(long, default_value = "file")]
        store: String,
    },
    /// Launch the browser directly (no automation driver) and keep it running.
    Test {
        /// Run headless.
        #[arg(long)]
        headless: bool,
        /// Proxy server (user:pass@host:port supported via auth extension).
        #[arg(long)]
        proxy_server: Option<String>,
    },
    /// Launch the browser driven by the native Juggler driver.
    Launch {
        /// URL to open (about:blank when omitted).
        url: Option<String>,
        /// Run headless.
        #[arg(long)]
        headless: bool,
        /// Proxy server (http/socks; credentials supported natively).
        #[arg(long)]
        proxy_server: Option<String>,
        /// Proxy username.
        #[arg(long)]
        proxy_username: Option<String>,
        /// Proxy password.
        #[arg(long)]
        proxy_password: Option<String>,
        /// Use a stored persona (see `camoufox persona`).
        #[arg(long)]
        persona: Option<String>,
        /// Persona store spec (file[:<dir>], sqlite[:<path>], mysql:<dsn>, s3://…).
        #[arg(long, default_value = "file")]
        store: String,
        /// Fingerprint seed for an ad-hoc persona (deterministic).
        #[arg(long)]
        seed: Option<u64>,
        /// Persistent profile directory (cookies/history survive restarts).
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Verify the fingerprint injection and print a report.
        #[arg(long)]
        verify: bool,
        /// Save a PNG screenshot to this path (after load).
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Dump the captured page content (HTML) to this path.
        #[arg(long)]
        dump_html: Option<PathBuf>,
        /// Record network traffic and export it as HAR 1.2 to this path.
        #[arg(long)]
        har: Option<PathBuf>,
        /// Save the session (cookies + local storage) for the persona.
        #[arg(long)]
        save_session: bool,
        /// Restore a previously saved session before navigating.
        #[arg(long)]
        restore_session: bool,
        /// Extra time (seconds) to keep the browser open after the commands.
        #[arg(long, default_value_t = 3)]
        hold: u64,
    },
    /// Launch the browser and verify the fingerprint injection end-to-end.
    Verify {
        /// Constrain the fingerprint OS.
        #[arg(long)]
        os: Option<String>,
        /// Use a stored persona.
        #[arg(long)]
        persona: Option<String>,
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
        /// Fingerprint seed for an ad-hoc persona (deterministic).
        #[arg(long)]
        seed: Option<u64>,
    },
    /// Manage persisted personas (stable identities keyed by seed).
    Persona {
        #[command(subcommand)]
        action: PersonaAction,
    },
}

#[derive(Subcommand)]
enum PersonaAction {
    /// Generate (or reuse) a persona from a seed and persist it.
    Generate {
        /// Persona id.
        id: String,
        /// Deterministic seed.
        #[arg(long)]
        seed: u64,
        /// Human-readable name.
        #[arg(long)]
        name: Option<String>,
        /// Constrain the fingerprint OS.
        #[arg(long)]
        os: Option<String>,
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
    },
    /// List stored personas.
    List {
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
    },
    /// Print a persona's fingerprint as JSON.
    Show {
        /// Persona id.
        id: String,
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
    },
    /// Delete a persona.
    Delete {
        /// Persona id.
        id: String,
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
    },
    /// Show the default store spec and where it resolves to.
    Where {
        /// Persona store spec.
        #[arg(long, default_value = "file")]
        store: String,
    },
}

pub fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let result = runtime.block_on(run(cli));
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Fetch => {
            camoufox_pkgman::install().await?;
            camoufox_geoip::download_mmdb().await?;
            let mut addons = Vec::new();
            camoufox_pkgman::add_default_addons(&mut addons, &[]).await?;
            println!("Camoufox is up to date.");
            Ok(())
        }
        Command::Remove => {
            match camoufox_pkgman::CamoufoxFetcher::cleanup() {
                Ok(true) => println!("Camoufox binaries removed!"),
                Ok(false) => println!("Camoufox binaries not found!"),
                Err(e) => return Err(e),
            }
            camoufox_geoip::remove_mmdb();
            Ok(())
        }
        Command::Path => {
            println!("{}", camoufox_pkgman::install_dir().display());
            Ok(())
        }
        Command::Version => {
            match camoufox_pkgman::installed_ver_str() {
                Ok(version) => println!("Camoufox:\tv{version}"),
                Err(_) => println!("Camoufox:\tNot downloaded!"),
            }
            println!(
                "Supported range:\t{}",
                camoufox_pkgman::version::Constraints::as_range()
            );
            Ok(())
        }
        Command::Prepare {
            os,
            headless,
            with_config,
            persona,
            store,
        } => {
            let mut options = build_options(os.as_deref(), headless)?;
            if let Some(id) = &persona {
                options.persona = Some(load_persona(&store, id).await?);
            }
            let prepared = prepare(&options).await?;
            let mut json = serde_json::to_value(&prepared)?;
            if !with_config {
                if let Some(object) = json.as_object_mut() {
                    object.remove("config");
                }
            }
            println!("{}", serde_json::to_string_pretty(&json)?);
            Ok(())
        }
        Command::Test {
            headless,
            proxy_server,
        } => {
            let mut options = build_options(None, headless)?;
            options.headless = if headless {
                HeadlessMode::On
            } else {
                HeadlessMode::Off
            };
            if let Some(server) = proxy_server {
                options.proxy = Some(parse_proxy_server(&server, None, None));
            }
            let mut browser = launch(&options).await?;
            println!(
                "Camoufox running (pid {:?}). Press Ctrl+C to stop.",
                browser.id()
            );
            let _ = browser.wait().await;
            Ok(())
        }
        Command::Launch {
            url,
            headless,
            proxy_server,
            proxy_username,
            proxy_password,
            persona,
            store,
            seed,
            profile,
            verify,
            screenshot,
            dump_html,
            har,
            save_session,
            restore_session,
            hold,
        } => {
            launch_command(LaunchArgs {
                url: url.as_deref(),
                headless,
                proxy_server: proxy_server.as_deref(),
                proxy_username: proxy_username.as_deref(),
                proxy_password: proxy_password.as_deref(),
                persona: persona.as_deref(),
                store: &store,
                seed,
                profile: profile.as_deref(),
                verify,
                screenshot: screenshot.as_deref(),
                dump_html: dump_html.as_deref(),
                har: har.as_deref(),
                save_session,
                restore_session,
                hold,
            })
            .await
        }
        Command::Verify {
            os,
            persona,
            store,
            seed,
        } => {
            let mut options = build_options(os.as_deref(), true)?;
            options.headless = HeadlessMode::On;
            if let Some(id) = &persona {
                options.persona = Some(load_persona(&store, id).await?);
            } else if let Some(seed) = seed {
                let record = adhoc_persona(&store, &format!("seed-{seed}"), seed, &options).await?;
                options.persona = Some(record);
            }
            verify_command(options).await
        }
        Command::Persona { action } => persona_command(action).await,
    }
}

/// Everything the `launch` subcommand needs.
struct LaunchArgs<'a> {
    url: Option<&'a str>,
    headless: bool,
    proxy_server: Option<&'a str>,
    proxy_username: Option<&'a str>,
    proxy_password: Option<&'a str>,
    persona: Option<&'a str>,
    store: &'a str,
    seed: Option<u64>,
    profile: Option<&'a std::path::Path>,
    verify: bool,
    screenshot: Option<&'a std::path::Path>,
    dump_html: Option<&'a std::path::Path>,
    har: Option<&'a std::path::Path>,
    save_session: bool,
    restore_session: bool,
    hold: u64,
}

async fn launch_command(args: LaunchArgs<'_>) -> Result<()> {
    let LaunchArgs {
        url,
        headless,
        proxy_server,
        proxy_username,
        proxy_password,
        persona,
        store,
        seed,
        profile,
        verify,
        screenshot,
        dump_html,
        har,
        save_session,
        restore_session,
        hold,
    } = args;

    let mut options = build_options(None, headless)?;
    options.headless = if headless {
        HeadlessMode::On
    } else {
        HeadlessMode::Off
    };
    if let Some(server) = proxy_server {
        options.proxy = Some(parse_proxy_server(server, proxy_username, proxy_password));
    }
    if let Some(id) = persona {
        options.persona = Some(load_persona(store, id).await?);
    } else if let Some(seed) = seed {
        let record = adhoc_persona(store, &format!("seed-{seed}"), seed, &options).await?;
        println!("persona: {} (seed {seed})", record.id);
        options.persona = Some(record);
    }
    if let Some(profile) = profile {
        options.persistent_profile = Some(profile.to_path_buf());
    }

    let mut browser = camoufox_juggler::launch_with_juggler(&options)
        .await
        .map_err(camoufox_juggler::core_error)?;
    println!("Camoufox running (pid {:?}).", browser.id());

    let page = browser
        .new_page()
        .await
        .map_err(camoufox_juggler::core_error)?;

    // HAR recording: a background task records network events while the
    // session runs (page commands pump the events into the stream).
    let har_recorder = har.map(|path| {
        let mut har = camoufox_juggler::har::HarLog::new();
        har.set_title(url.unwrap_or("camoufox session"));
        har.start_page(url.unwrap_or("about:blank"));
        let mut events = page.network_events();
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(har));
        let task_har = shared.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = events.next().await.unwrap_or(None) {
                task_har.lock().await.record(&event);
            }
        });
        (shared, task, path)
    });

    // Session restore (cookies first, then local storage per origin).
    let persona_id = options
        .persona
        .as_ref()
        .map(|record| record.id.clone())
        .unwrap_or_else(|| "default".to_string());
    if restore_session {
        let persistence = SessionPersistence::new(open_store(store).await?);
        if let Some(snapshot) = persistence.load(&persona_id).await? {
            restore_snapshot(&page, &snapshot).await?;
            println!(
                "restored session: {} cookies, {} local-storage origins",
                snapshot.cookies.len(),
                snapshot.local_storage.len()
            );
        } else {
            println!("no saved session for persona '{persona_id}'");
        }
    }

    let target = url.unwrap_or("about:blank");
    page.goto(target)
        .await
        .map_err(camoufox_juggler::core_error)?;
    println!("navigated to {target}");

    if verify {
        let report = camoufox_juggler::verify_fingerprint(&page, &browser.prepared.config)
            .await
            .map_err(camoufox_juggler::core_error)?;
        print!("{}", report.render());
    }
    if let Some(path) = dump_html {
        let html = page.content().await.map_err(camoufox_juggler::core_error)?;
        std::fs::write(path, html)?;
        println!("HTML dumped to {}", path.display());
    }
    if let Some(path) = screenshot {
        page.screenshot(path)
            .await
            .map_err(camoufox_juggler::core_error)?;
        println!("screenshot saved to {}", path.display());
    }
    if save_session {
        let snapshot = capture_snapshot(&page, &persona_id).await?;
        let persistence = SessionPersistence::new(open_store(store).await?);
        persistence.save(&snapshot).await?;
        println!(
            "saved session: {} cookies, {} local-storage origins",
            snapshot.cookies.len(),
            snapshot.local_storage.len()
        );
    }

    if hold > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(hold)).await;
    }

    // Flush the HAR recorder (network events observed during the session).
    if let Some((shared, task, path)) = har_recorder {
        // Give the recorder a moment to drain the last events.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        task.abort();
        let har = shared.lock().await;
        har.write_to(path)
            .await
            .map_err(camoufox_juggler::core_error)?;
        println!("HAR exported ({} entries) to {}", har.len(), path.display());
    }

    browser
        .close()
        .await
        .map_err(camoufox_juggler::core_error)?;
    println!("browser closed.");
    Ok(())
}

async fn verify_command(options: LaunchOptions) -> Result<()> {
    let mut browser = camoufox_juggler::launch_with_juggler(&options)
        .await
        .map_err(camoufox_juggler::core_error)?;
    let page = browser
        .new_page()
        .await
        .map_err(camoufox_juggler::core_error)?;
    page.goto("about:blank")
        .await
        .map_err(camoufox_juggler::core_error)?;
    let report = camoufox_juggler::verify_fingerprint(&page, &browser.prepared.config)
        .await
        .map_err(camoufox_juggler::core_error)?;
    print!("{}", report.render());
    browser
        .close()
        .await
        .map_err(camoufox_juggler::core_error)?;
    if report.passed() {
        Ok(())
    } else {
        Err(camoufox_core::error::CamoufoxError::Juggler(
            "fingerprint verification failed".into(),
        ))
    }
}

async fn persona_command(action: PersonaAction) -> Result<()> {
    match action {
        PersonaAction::Generate {
            id,
            seed,
            name,
            os,
            store,
        } => {
            let options = build_options(os.as_deref(), false)?;
            let record = adhoc_persona(&store, &id, seed, &options).await?;
            let record = match name {
                Some(name) => {
                    let mut record = record;
                    record.name = Some(name);
                    record
                }
                None => record,
            };
            let store_handle = PersonaStore::new(open_store(&store).await?);
            store_handle.save(&record).await?;
            println!(
                "persona '{}' saved (seed {seed}, {})",
                record.id, record.fingerprint.fingerprint.navigator.user_agent
            );
            Ok(())
        }
        PersonaAction::List { store } => {
            let store_handle = PersonaStore::new(open_store(&store).await?);
            let summaries = store_handle.list().await?;
            if summaries.is_empty() {
                println!("no personas stored (spec: {store})");
                return Ok(());
            }
            println!(
                "{:<24} {:<8} {:<16} {:<10}",
                "ID", "SEED", "CREATED", "USER AGENT"
            );
            for summary in summaries {
                let created = chrono_less(summary.created_at);
                println!(
                    "{:<24} {:<8} {:<16} {:<10}",
                    summary.id,
                    summary.seed.map(|s| s.to_string()).unwrap_or_default(),
                    created,
                    summary.user_agent
                );
            }
            Ok(())
        }
        PersonaAction::Show { id, store } => {
            let record = load_persona(&store, &id).await?;
            println!("{}", serde_json::to_string_pretty(&record)?);
            Ok(())
        }
        PersonaAction::Delete { id, store } => {
            let store_handle = PersonaStore::new(open_store(&store).await?);
            if store_handle.delete(&id).await? {
                println!("persona '{id}' deleted");
            } else {
                println!("persona '{id}' not found");
            }
            Ok(())
        }
        PersonaAction::Where { store } => {
            println!("spec: {store}");
            match camoufox_store::ProviderSpec::parse(&store) {
                Ok(camoufox_store::ProviderSpec::Memory) => println!("resolves: memory"),
                Ok(camoufox_store::ProviderSpec::File(dir)) => {
                    println!("resolves: file store at {}", dir.display())
                }
                Ok(camoufox_store::ProviderSpec::Sqlite(path)) => {
                    println!("resolves: sqlite store at {}", path.display())
                }
                Ok(camoufox_store::ProviderSpec::Mysql(_)) => {
                    println!("resolves: mysql store (connects on first use)")
                }
                Ok(camoufox_store::ProviderSpec::S3 { .. }) => {
                    println!("resolves: s3 store (credentials from the AWS environment)")
                }
                Ok(camoufox_store::ProviderSpec::Custom(_)) => {
                    println!("resolves: custom provider")
                }
                Err(e) => return Err(e),
            }
            Ok(())
        }
    }
}

async fn load_persona(store: &str, id: &str) -> Result<camoufox_core::persona::PersonaRecord> {
    let store_handle = PersonaStore::new(open_store(store).await?);
    store_handle.require(id).await
}

async fn adhoc_persona(
    store: &str,
    id: &str,
    seed: u64,
    options: &LaunchOptions,
) -> Result<camoufox_core::persona::PersonaRecord> {
    let request = camoufox_core::fingerprint::FingerprintRequest {
        window: options.window,
        operating_systems: if options.os.is_empty() {
            None
        } else {
            Some(options.os.clone())
        },
        screen: options.screen,
        seed: Some(seed),
    };
    let store_handle = PersonaStore::new(open_store(store).await?);
    store_handle
        .get_or_generate(id, None::<String>, request)
        .await
}

/// Captures cookies + current-origin local storage into a snapshot.
async fn capture_snapshot(
    page: &camoufox_juggler::JugglerPage,
    persona_id: &str,
) -> Result<SessionSnapshot> {
    let mut snapshot = SessionSnapshot::new(persona_id);

    let cookies = page.cookies().await.map_err(camoufox_juggler::core_error)?;
    for cookie in cookies {
        snapshot.cookies.push(PersonaCookie {
            name: string_field(&cookie, "name"),
            value: string_field(&cookie, "value"),
            domain: string_field(&cookie, "domain"),
            path: string_field(&cookie, "path"),
            expires: cookie
                .get("expires")
                .and_then(serde_json::Value::as_f64)
                .filter(|expires| *expires > 0.0),
            secure: cookie
                .get("secure")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            http_only: cookie
                .get("httpOnly")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            same_site: cookie
                .get("sameSite")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }

    if let Some((origin, entries)) = page
        .local_storage()
        .await
        .map_err(camoufox_juggler::core_error)?
    {
        if !entries.is_empty() {
            snapshot
                .local_storage
                .push(PersonaLocalStorage { origin, entries });
        }
    }
    Ok(snapshot)
}

/// Restores a snapshot: cookies through the protocol, local storage per
/// origin (requires navigating to each origin).
async fn restore_snapshot(
    page: &camoufox_juggler::JugglerPage,
    snapshot: &SessionSnapshot,
) -> Result<()> {
    if !snapshot.cookies.is_empty() {
        let cookies: Vec<serde_json::Value> = snapshot
            .cookies
            .iter()
            .map(|cookie| {
                let mut value = serde_json::json!({
                    "name": cookie.name,
                    "value": cookie.value,
                    "domain": cookie.domain,
                    "path": cookie.path,
                });
                if cookie.secure {
                    value["secure"] = serde_json::Value::Bool(true);
                }
                if cookie.http_only {
                    value["httpOnly"] = serde_json::Value::Bool(true);
                }
                if let Some(same_site) = &cookie.same_site {
                    value["sameSite"] = serde_json::Value::String(same_site.clone());
                }
                if let Some(expires) = cookie.expires {
                    // Firefox caps cookie expiry at ~400 days and silently
                    // drops anything beyond; clamp to stay under the cap.
                    let cap = now_seconds() + 399 * 86400;
                    value["expires"] = serde_json::json!(expires.min(cap as f64));
                }
                value
            })
            .collect();
        page.set_cookies(&cookies)
            .await
            .map_err(camoufox_juggler::core_error)?;
    }

    for storage in &snapshot.local_storage {
        let url = if storage.origin.ends_with('/') {
            storage.origin.clone()
        } else {
            format!("{}/", storage.origin)
        };
        page.goto(&url)
            .await
            .map_err(camoufox_juggler::core_error)?;
        page.set_local_storage(&storage.entries)
            .await
            .map_err(camoufox_juggler::core_error)?;
    }
    Ok(())
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Current unix time in seconds.
fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn parse_proxy_server(server: &str, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
    // user:pass@host:port embedded in the URL.
    let (embedded_user, embedded_pass, server) = match server.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((credentials, host)) => {
                let (user, pass) = credentials
                    .split_once(':')
                    .map(|(u, p)| (u.to_string(), p.to_string()))
                    .unwrap_or((credentials.to_string(), String::new()));
                (Some(user), Some(pass), format!("{scheme}://{host}"))
            }
            None => (None, None, server.to_string()),
        },
        None => (None, None, server.to_string()),
    };
    ProxyConfig {
        server,
        username: username.map(str::to_string).or(embedded_user),
        password: password.map(str::to_string).or(embedded_pass),
        bypass: None,
    }
}

/// Minimal unix timestamp → `YYYY-MM-DD` renderer (no chrono dependency).
fn chrono_less(seconds: u64) -> String {
    let days = (seconds / 86400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn build_options(os: Option<&str>, headless: bool) -> Result<LaunchOptions> {
    let mut options = LaunchOptions {
        headless: if headless {
            HeadlessMode::On
        } else {
            HeadlessMode::Off
        },
        ..Default::default()
    };
    if let Some(os) = os {
        let supported = ["windows", "macos", "linux"]
            .iter()
            .find(|candidate| *candidate == &os)
            .map(|candidate| match *candidate {
                "windows" => SupportedOs::Windows,
                "macos" => SupportedOs::Macos,
                _ => SupportedOs::Linux,
            })
            .ok_or_else(|| {
                camoufox_core::error::CamoufoxError::InvalidOs(format!(
                    "unsupported OS '{os}' (expected windows, macos or linux)"
                ))
            })?;
        options.os = vec![supported];
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_server_parsing() {
        let proxy = parse_proxy_server("http://user:pass@host:3128", None, None);
        assert_eq!(proxy.server, "http://host:3128");
        assert_eq!(proxy.username.as_deref(), Some("user"));
        assert_eq!(proxy.password.as_deref(), Some("pass"));

        let proxy = parse_proxy_server("socks5://host:1080", Some("u"), Some("p"));
        assert_eq!(proxy.server, "socks5://host:1080");
        assert_eq!(proxy.username.as_deref(), Some("u"));

        let proxy = parse_proxy_server("host:3128", None, None);
        assert_eq!(proxy.server, "host:3128");
        assert!(proxy.username.is_none());
    }

    #[test]
    fn date_rendering() {
        assert_eq!(chrono_less(0), "1970-01-01");
        assert_eq!(chrono_less(1_769_766_161), "2026-01-30");
    }
}
