# camoufox-rust

Rust launcher and toolbox for the [Camoufox](https://camoufox.com) anti-detect
browser. Manages the browser binaries, generates statistically realistic
Firefox fingerprints, and prepares/launches the browser with full fingerprint,
locale, geolocation and WebGL spoofing.

Inspired by camoufox-js (https://github.com/apify/camoufox-js) and Camoufox (https://github.com/daijro/camoufox).

## Workspace layout

| Crate | Role |
|---|---|
| `camoufox-core` | Pure domain: errors, OS types, fingerprint generation, config validation/seeding/chunking, statistical locale selection (embedded CLDR data), mapping tables |
| `camoufox-pkgman` | GitHub release discovery, version constraints, download with progress, zip extraction, default addon provisioning (uBlock Origin) |
| `camoufox-geoip` | Public IP resolution (proxy-aware), MaxMind GeoLite2-City geolocation |
| `camoufox-webgl` | Weighted WebGL fingerprint sampling from an embedded SQLite database |
| `camoufox-virtdisplay` | Xvfb `-displayfd` virtual display management (Linux) |
| `camoufox` | Facade: `LaunchOptions` → `PreparedLaunch` → `launch()`, plus the CLI |

Fingerprint generation is backed by
[`veilus-fingerprint`](https://crates.io/crates/veilus-fingerprint) (Bayesian
networks, browserforge-compatible), constrained to coherent desktop Firefox
profiles.

## CLI

```bash
# Download browser binaries, GeoIP database and addons
camoufox fetch

# Print install dir / installed version
camoufox path
camoufox version

# Resolve launch options and print everything a driver needs (JSON)
camoufox prepare --os linux [--headless] [--with-config]

# Launch the browser directly and keep it running
camoufox test [--headless]
```

## Library

```rust
use camoufox::builder::{prepare, LaunchOptions, HeadlessMode};
use camoufox_core::os::SupportedOs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = LaunchOptions {
        os: vec![SupportedOs::Linux],
        headless: HeadlessMode::On,
        ..Default::default()
    };

    // Option A: prepare and hand off to any Playwright driver
    let prepared = prepare(&options).await?;
    println!("executable: {}", prepared.executable_path.display());
    // prepared.env holds the CAMOU_CONFIG_* chunks + FONTCONFIG_PATH
    // prepared.firefox_user_prefs goes into the profile's user.js
    // prepared.spoofs_window_dimensions == true → use viewport: null

    // Option B: launch the browser process directly
    // let mut browser = camoufox::launch::launch(&options).await?;

    Ok(())
}
```

## Environment

- `CAMOUFOX_INSTALL_DIR` — relocate the browser install directory
- `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD` — skip downloads (CI with pre-provisioned binaries)
- `CAMOUFOX_DEBUG` — verbose public-IP resolution failures

No authentication is needed: release discovery and downloads hit the public
GitHub API (60 requests/hour per IP unauthenticated).

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets  # 0 warnings
```

Requires Rust 1.75+. The browser binaries (~600MB) are downloaded on demand
into the per-user cache directory.

## Roadmap

Features planned for future releases:

1. **Native Juggler/CDP driver** — a Rust client for Firefox's Juggler
   automation protocol, closing the automation loop without any Playwright
   dependency (also a prerequisite for verified fingerprint injection)
2. **Fingerprint injection verification** — integration tests that assert the
   running browser's `navigator.userAgent` (and other spoofed surfaces)
   actually match the generated fingerprint
3. **Cookie/storage persistence API** — reuse profiles, cookies and local
   storage across sessions (session keep-alive for logged-in flows)
4. **Authenticated proxy support** — a Firefox-compatible proxy-auth
   extension so `--proxy-server` works with `user:pass@host` credentials
   without an external driver
5. **Fingerprint cache** — persist generated identities keyed by seed,
   enabling stable personas across runs

