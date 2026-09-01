//! Command-line interface: fetch, remove, path, version, prepare, test.

use clap::{Parser, Subcommand};

use camoufox::builder::{prepare, HeadlessMode, LaunchOptions};
use camoufox::launch::launch;
use camoufox_core::error::Result;
use camoufox_core::os::SupportedOs;

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
    },
    /// Launch the browser and keep it running until interrupted.
    Test {
        /// URL to open.
        url: Option<String>,
        /// Run headless.
        #[arg(long)]
        headless: bool,
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
        } => {
            let options = build_options(os.as_deref(), headless)?;
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
        Command::Test { url, headless } => {
            let mut options = build_options(None, headless)?;
            if headless {
                options.headless = HeadlessMode::On;
            }
            if url.is_some() {
                eprintln!("note: opening URLs requires an automation driver; the browser will start idle.");
            }
            let mut browser = launch(&options).await?;
            println!(
                "Camoufox running (pid {:?}). Press Ctrl+C to stop.",
                browser.id()
            );
            let _ = browser.wait().await;
            Ok(())
        }
    }
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
