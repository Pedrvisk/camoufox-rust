# camoufox-rust

Rust launcher and toolbox for the [Camoufox](https://camoufox.com) anti-detect
browser. Manages the browser binaries, generates statistically realistic
Firefox fingerprints, and prepares/launches the browser with full fingerprint,
locale, geolocation and WebGL spoofing — plus a native automation driver,
persona persistence and authenticated proxy support.

Inspired by [camoufox-js](https://github.com/apify/camoufox-js) and [Camoufox](https://github.com/daijro/camoufox).

## Workspace layout

| Crate                  | Role                                                                                                                                                         |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `camoufox-core`        | Pure domain: errors, OS types, fingerprint generation, config validation/seeding/chunking, statistical locale selection (embedded CLDR data), mapping tables, persona data model |
| `camoufox-pkgman`      | GitHub release discovery, version constraints, download with progress, zip extraction, default addon provisioning (uBlock Origin)                            |
| `camoufox-geoip`       | Public IP resolution (proxy-aware), MaxMind GeoLite2-City geolocation                                                                                        |
| `camoufox-webgl`       | Weighted WebGL fingerprint sampling from an embedded SQLite database                                                                                         |
| `camoufox-virtdisplay` | Xvfb `-displayfd` virtual display management (Linux)                                                                                                         |
| `camoufox-store`       | Pluggable persistence for personas and sessions: memory, file, SQLite, MySQL and S3 providers                                                                |
| `camoufox-juggler`     | Native Rust client for Firefox's Juggler automation protocol (pipe transport, browser/page sessions, fingerprint verification)                               |
| `camoufox`             | Facade: `LaunchOptions` → `PreparedLaunch` → `launch()`, proxy-auth WebExtension, persistent profiles                                                         |
| `camoufox-cli`         | The `camoufox` binary: fetch/prepare/test/launch/verify + persona management                                                                                 |

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
camoufox prepare --os linux [--headless] [--with-config] [--persona <id>]

# Launch the browser directly (no automation driver)
camoufox test [--headless] [--proxy-server http://user:pass@host:port]

# Launch driven by the native Juggler driver
camoufox launch https://example.com --headless \
  [--persona <id>] [--seed <n>] [--profile <dir>] \
  [--verify] [--screenshot out.png] [--dump-html page.html] \
  [--save-session] [--restore-session] \
  [--proxy-server http://user:pass@host:port]

# Verify the fingerprint injection end-to-end (exits non-zero on mismatch)
camoufox verify [--os linux] [--persona <id>] [--seed <n>]

# Manage persisted personas (stable identities keyed by seed)
camoufox persona generate <id> --seed 42 [--name "work"] [--store sqlite]
camoufox persona list [--store sqlite]
camoufox persona show <id>
camoufox persona delete <id>
camoufox persona where
```

### Persona stores

`--store` accepts a spec (or set it globally via `CAMOUFOX_PERSONA_STORE`):

| Spec                        | Backend                                                |
| --------------------------- | ------------------------------------------------------ |
| `memory`                    | in-process (throwaway)                                 |
| `file` / `file:<dir>`       | JSON documents (default: `~/.cache/camoufox/personas`) |
| `sqlite` / `sqlite:<path>`  | single-file SQLite database                            |
| `mysql:<dsn>`               | shared MySQL database (`mysql://user:pass@host/db`)    |
| `s3://bucket/prefix?region=sa-east-1&endpoint=…` | S3-compatible object storage (AWS/MinIO/R2, SigV4) |

Custom backends implement the `camoufox_store::StorageProvider` trait.

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

    // Option C: native automation — no Playwright at all
    // let mut browser = camoufox_juggler::launch_with_juggler(&options).await?;
    // let page = browser.new_page().await?;
    // page.goto("https://example.com").await?;

    Ok(())
}
```

### Fingerprint verification

```rust
let mut browser = camoufox_juggler::launch_with_juggler(&options).await?;
let page = browser.new_page().await?;
page.goto("about:blank").await?;
let report = camoufox_juggler::verify_fingerprint(&page, &browser.prepared.config).await?;
assert!(report.passed());
println!("{}", report.render());
```

### Personas with pluggable persistence

```rust
use camoufox_store::{open, PersonaStore};
use camoufox_core::FingerprintRequest;

let store = PersonaStore::new(open("sqlite:/var/lib/camoufox/personas.sqlite")?);
// Deterministic identity, generated once and reused across runs
let record = store
    .get_or_generate("work-account", Some("Work"), FingerprintRequest {
        seed: Some(42),
        ..Default::default()
    })
    .await?;

let options = LaunchOptions {
    persona: Some(record),
    persistent_profile: Some("/var/lib/camoufox/profile-work".into()),
    ..Default::default()
};
```

## Authenticated proxies

Firefox ignores credentials in `--proxy-server`. Two native paths make
`user:pass@host:port` work:

- **Juggler driver** — credentials go to `Browser.setBrowserProxy`; the
  protocol answers the proxy auth prompts itself.
- **Direct launch** (`camoufox test`) — a generated proxy-auth WebExtension
  (`camoufox::proxyauth`) configures the proxy through `proxy.onRequest` and
  answers the auth challenge through `webRequest.onAuthRequired`. No external
  driver needed.

## Environment

- `CAMOUFOX_INSTALL_DIR` — relocate the browser install directory
- `CAMOUFOX_PERSONA_STORE` — default persona store spec
- `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD` — skip downloads (CI with pre-provisioned binaries)
- `CAMOUFOX_DEBUG` — verbose public-IP resolution failures
- `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_REGION` — S3 store credentials

No authentication is needed for browser downloads: release discovery and
downloads hit the public GitHub API (60 requests/hour per IP unauthenticated).

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets  # 0 warnings
```

Requires Rust 1.75+. The browser binaries (~600MB) are downloaded on demand
into the per-user cache directory.

Integration tests launch the real browser and are skipped when it is not
installed — run `camoufox fetch` first.

## Notes

- The Juggler pipe transport is Unix-only for now.
- Firefox caps cookie expiry at ~400 days; session restore clamps far-future
  cookies to stay under the cap.
- With a persistent profile (`persistent_profile`), pages run in the default
  browser context so cookies/local storage/history land in the profile
  directory and survive restarts.

## Roadmap

Implemented in this release:

1. **Native Juggler/CDP driver** — `camoufox-juggler` speaks Firefox's
   Juggler protocol over its pipe transport (NUL-delimited JSON on FDs 3/4),
   closing the automation loop with zero Playwright dependency
2. **Fingerprint injection verification** — `camoufox verify` /
   `verify_fingerprint` asserts the running browser's `navigator.userAgent`,
   platform, oscpu, screen dimensions, … match the generated fingerprint
   (live integration test included)
3. **Cookie/storage persistence API** — persistent profiles (`user.js`
   re-materialized each launch, default-context pages) plus session
   snapshots (cookies + local storage) saved/restored per persona
4. **Authenticated proxy support** — credentials via
   `Browser.setBrowserProxy` (driver path) or a generated proxy-auth
   WebExtension (driverless path)
5. **Fingerprint cache** — personas persisted keyed by seed through
   pluggable storage providers (file / SQLite / MySQL / S3 / custom),
   enabling stable personas across runs

Ideas for future releases:

- Windows support for the Juggler pipe transport
- Request interception and network event APIs surfaced in `JugglerPage`
- Persona rotation policies (per-domain, time-based)
