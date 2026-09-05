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
  [--har session.har] \
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

### Network events & request interception

```rust
use camoufox_juggler::NetworkEvent;

// Observe traffic
let mut events = page.network_events();

// Intercept: block ad domains, pass everything else through
page.set_request_interception(true).await?;
while let Some(event) = events.next().await? {
    if let NetworkEvent::RequestWillBeSent(request) = event {
        if request.is_intercepted {
            if request.url.contains("ads.example") {
                page.take_intercepted_request(&request).abort().await?;
            } else {
                let route = page.take_intercepted_request(&request);
                route.continue_request(Default::default()).await?;
            }
        }
    }
}
```

### Persona rotation policies

```rust
use camoufox_core::rotation::{RotationContext, RotationPolicy, RotationState};

let policy = RotationPolicy::Any {
    policies: vec![
        RotationPolicy::PerDomain,                       // sticky per-site identity
        RotationPolicy::TimeBased { max_age_secs: 86400 }, // rotate daily
        RotationPolicy::UsageBased { max_uses: 100 },     // …or after 100 launches
    ],
};
let decision = policy.decide(&RotationContext {
    current: current_persona.as_ref(),
    pool: &personas,
    state: &rotation_state,
    domain: Some("example.com"),
    persona_domains: &domain_assignments,
});
```

### Multi-browser orchestration

```rust
use camoufox_juggler::orchestrator::{Orchestrator, OrchestratorOptions};
use camoufox_core::rotation::RotationPolicy;

let orchestrator = Orchestrator::new(OrchestratorOptions {
    base_options: LaunchOptions::default(),
    store_spec: "sqlite".into(),
    policy: RotationPolicy::PerDomain,
    concurrency: 4,
}).await?;

// Each domain gets its own browser + persona; the rotation state
// (use counters, domain assignments) persists in the store.
let sessions = vec![
    orchestrator.launch_for_domain("a.example").await?,
    orchestrator.launch_for_domain("b.example").await?,
];
for session in sessions {
    // session.browser / session.persona / session.domain
    session.close().await?;
}
```

### HAR export

```rust
use camoufox_juggler::har::HarLog;

let mut har = HarLog::new();
let mut events = page.network_events();
while let Some(event) = events.next().await? {
    if let camoufox_juggler::NetworkEvent::ResponseReceived(response) = &event {
        if let Ok(body) = page.response_body(&response.request_id).await {
            har.attach_body(&response.request_id, body);
        }
    }
    har.record(&event);
}
har.write_to(std::path::Path::new("session.har")).await?;
```

### Input (mouse, keyboard, touch, wheel)

```rust
// Mouse
page.click(320.0, 240.0).await?;
page.double_click(320.0, 240.0).await?;
page.click_with(10.0, 10.0, MouseButton::Right, Modifiers::SHIFT).await?;

// Keyboard
page.type_text("hello world").await?;
page.press_key_with("a", Modifiers::CTRL).await?; // select all

// Scrolling
page.wheel(320.0, 240.0, 0.0, -600.0).await?; // scroll up

// Touch
page.tap(320.0, 240.0).await?;
page.touch_event(TouchEventType::Start, &[TouchPoint { x: 100.0, y: 100.0 }]).await?;
```

### Downloads

```rust
use camoufox_juggler::{DownloadBehavior, DownloadEvent};

// Save downloads to a directory and watch them.
browser.set_download_options(Some(&DownloadBehavior::SaveToDisk("downloads".into()))).await?;
let mut downloads = browser.download_events();
while let Some(event) = downloads.next().await {
    match event {
        DownloadEvent::Created(created) => {
            println!("download started: {}", created.suggested_file_name);
        }
        DownloadEvent::Finished(finished) => {
            println!("download finished: {}", finished.uuid);
            break;
        }
    }
}

// Abort everything instead:
browser.set_download_options(Some(&DownloadBehavior::Cancel)).await?;
```

### WebSocket injection

```rust
// Before the page opens its sockets:
page.enable_websocket_injection().await?;
page.goto("https://example.com/chat").await?;

// Send a frame as the page (client → server):
page.send_websocket_message("wss://example.com/chat", r#"{"type":"ping"}"#).await?;

// Inspect live sockets (url, readyState):
for (url, state) in page.live_websockets().await? {
    println!("{url}: readyState={state}");
}
```

### Screencast

```rust
// Subscribe before starting so no frame is missed.
let mut frames = page.screencast_frames();
page.start_screencast(1280, 720, 90).await?;

while let Some(frame) = frames.next().await {
    // frame.data: JPEG bytes; frame.device_width/height; frame.timestamp
    std::fs::write("frame.jpg", &frame.data)?;
    break; // one frame is enough for this demo
}
page.stop_screencast().await?;
```

### File chooser interception

```rust
page.set_intercept_file_chooser(true).await?;

// Trigger a click on the <input type=file> element…
page.click(50.0, 120.0).await?;

// …then feed files instead of the OS dialog.
let chooser = page.wait_for_file_chooser(Duration::from_secs(5)).await?;
chooser.set_files(&["C:\\uploads\\avatar.png"]).await?;
```

### Workers

```rust
use camoufox_juggler::WorkerEvent;

let mut events = page.worker_events();
page.goto("https://example.com/app-with-workers").await?;

while let Some(event) = events.next().await {
    match event {
        WorkerEvent::Created(info) => {
            println!("worker created: {} ({})", info.worker_id, info.url);
        }
        WorkerEvent::Message { worker_id, message } => {
            println!("worker {worker_id} says: {message}");
        }
        WorkerEvent::Destroyed { worker_id } => {
            println!("worker destroyed: {worker_id}");
        }
    }
}
```

### Emulation (touch & media)

```rust
use camoufox_juggler::emulation::{EmulatedMedia, MediaType, ReducedMotion};

// Make the page's context behave as a touch device.
page.set_touch_override(Some(true)).await?;

// Dark mode / print styles / accessibility emulation.
page.emulate_media(&EmulatedMedia::dark_mode()).await?;
page.emulate_media(&EmulatedMedia::print()).await?;
page.emulate_media(
    &EmulatedMedia::default()
        .with_media_type(MediaType::Screen)
        .with_reduced_motion(ReducedMotion::Reduce),
).await?;

// Back to browser defaults.
page.emulate_media(&EmulatedMedia::reset()).await?;
page.set_touch_override(None).await?;
```

### DOM helpers

```rust
// Element geometry by selector (None when it matches nothing).
if let Some(quads) = page.element_quads("button#submit").await? {
    let (x, y, width, height) = quads[0].bounding_box();
    println!("button at ({x}, {y}) {width}x{height}");
    let center = quads[0].center();
    page.click(center.x, center.y).await?;
}

// Or by object id, with scroll-into-view first.
if let Some(object_id) = page.query_object_id("iframe.widget").await? {
    page.scroll_into_view(&object_id).await?;
    let description = page.describe_node(&object_id).await?;
    if let Some(frame_id) = description.content_frame_id {
        println!("iframe's frame: {frame_id}");
    }
}
```

### Console & crash observability

```rust
let mut console = page.console_messages();
page.goto("https://example.com").await?;

while let Some(message) = console.next().await {
    if message.level == camoufox_juggler::ConsoleLevel::Error {
        println!(
            "console error at {}:{}: {}",
            message.url, message.line, message.text()
        );
    }
}

// Tab crash / close observability:
if page.is_crashed() { /* renderer died */ }
page.wait_for_close(std::time::Duration::from_secs(30)).await?;
```

### Window management, bindings, permissions & headers

```rust
use camoufox_juggler::Permission;

// Viewport / zoom / tab focus.
page.set_viewport_size(1280, 720).await?;
page.set_zoom(1.5).await?;
page.bring_to_front().await?;

// Expose a function into the page and await its calls.
let mut calls = page.binding_calls();
page.add_binding("rustReady").await?;
page.evaluate("rustReady({ state: 'loaded', url: location.href })").await?;
if let Some(call) = calls.next().await {
    println!("page says: {}", call.payload["state"]);
}

// Grant geolocation + notifications for an origin.
browser.grant_permissions(
    None, // default context
    "https://example.com",
    &[Permission::Geolocation, Permission::DesktopNotification],
).await?;

// Extra headers (context-wide) and cache control.
browser.set_extra_http_headers(None, &[("X-Custom".into(), "1".into())]).await?;
browser.clear_cache().await?;
browser.set_cache_disabled(None, true).await?;
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

- The Juggler pipe transport works on Unix (FDs 3/4) and Windows
  (`PW_PIPE_READ`/`PW_PIPE_WRITE` inheritable handles).
- Firefox caps cookie expiry at ~400 days; session restore clamps far-future
  cookies to stay under the cap.
- With a persistent profile (`persistent_profile`), pages run in the default
  browser context so cookies/local storage/history land in the profile
  directory and survive restarts.

## Roadmap

Implemented in this release:

1. **Native Juggler/CDP driver** — `camoufox-juggler` speaks Firefox's
   Juggler protocol over its pipe transport (NUL-delimited JSON),
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
6. **Windows support for the Juggler pipe transport** — the transport now
   creates inheritable anonymous pipes and passes them through the
   `PW_PIPE_READ`/`PW_PIPE_WRITE` environment variables (the mechanism
   Firefox's Juggler patch expects on Windows), so `launch_with_juggler`
   works on Windows as well as Unix
7. **Network events & request interception** — `page.network_events()`
   streams typed `Network.*` events (requests, responses, failures,
   timings) and `page.set_request_interception(true)` routes requests
   through `InterceptedRequest` handles: continue (with URL/method/
   header/body overrides), fulfill (synthetic responses) or abort
8. **Persona rotation policies** — `camoufox_core::rotation` decides when
   to move to a fresh identity: per-domain (sticky site↔persona binding,
   fresh persona per new site), time-based (max persona age) and
   usage-based (max launches per persona), combinable with `Any`
9. **WebSocket monitoring** — `NetworkEvent::WebSocket*` variants surface
   socket lifecycle (created/opened/closed) and frames (sent/received,
   opcode + payload) from `Page.webSocket*` events
10. **HAR export** — `camoufox_juggler::har::HarLog` records network events
    into a HAR 1.2 document (pages, entries, request/response headers,
    bodies, query strings, WebSocket messages); the CLI's
    `camoufox launch --har session.har` records traffic automatically
11. **Multi-browser orchestration** — `camoufox_juggler::orchestrator`
    runs a pool of persona-driven browsers: per-domain session launching,
    rotation-policy-driven persona selection, and rotation state (use
    counters + domain assignments) persisted in the persona store
12. **Input APIs** — `JugglerPage` dispatches synthesized input events:
    mouse (`click`, `double_click`, `mouse_down/move/up`), keyboard
    (`press_key`, `type_text`, `key_down/up` with modifier support),
    touch (`tap`, `touch_event`) and scrolling (`wheel`), through
    `Page.dispatch*Event` / `Page.insertText`
13. **Download management** — `browser.set_download_options` configures
    `saveToDisk`/`cancel` behavior, `browser.download_events()` streams
    `downloadCreated`/`downloadFinished` and `browser.cancel_download`
    aborts in-flight downloads
14. **WebSocket message injection** — `page.enable_websocket_injection()`
    installs a hook that registers live sockets;
    `page.send_websocket_message(url, text)` (or `send_websocket_binary`)
    sends client→server frames as the page, and `page.live_websockets()`
    lists open sockets with their readyState
15. **Screencast streaming** — `page.start_screencast(width, height,
    quality)` starts JPEG frame capture; `page.screencast_frames()`
    delivers decoded frames (with device dimensions and timestamps),
    auto-acked through `Page.screencastFrameAck` so capture keeps flowing
16. **File chooser interception** — `page.set_intercept_file_chooser(true)`
    intercepts `<input type=file>` clicks; `page.wait_for_file_chooser()`
    returns a `FileChooser` handle whose `set_files(&[paths])` feeds
    absolute local paths into the input (`Page.setFileInputFiles`)
17. **Worker messaging** — `page.workers()` lists live web workers,
    `page.worker_events()` streams created/destroyed/message events
    (workers are torn down when their frame navigates), and
    `page.send_message_to_worker(id, msg)` tunnels messages through
    `Page.sendMessageToWorker` (the channel Playwright uses to drive a
    full Juggler session inside workers; payloads are conventionally JSON
    strings)
18. **Touch capability override** — `browser.set_touch_override(ctx,
    has_touch)` / `page.set_touch_override(has_touch)` toggle a context's
    touch capability (`Browser.setTouchOverride`), making content behave
    as a touch device (`pointer: coarse`, DOM touch events) — the missing
    piece for full mobile emulation alongside the input APIs
19. **Emulated media & color scheme** — `page.emulate_media(&media)`
    emulates media type (`screen`/`print`), color scheme, reduced motion,
    forced colors and contrast (`Page.setEmulatedMedia`), with
    `EmulatedMedia` presets (`dark_mode`, `print`, `reset`) and builder
    methods
20. **DOM helpers** — `page.query_object_id(selector)` resolves elements
    to remote object ids; `page.content_quads` / `page.element_quads`
    fetch layout geometry (`Page.getContentQuads`, with
    `bounding_box`/`center` helpers for click targeting);
    `page.scroll_into_view`, `page.describe_node` (owner/content frame)
    and `page.adopt_node` (cross-context node adoption)
21. **Console capture** — `page.console_messages()` streams
    `Runtime.console` events as typed `ConsoleMessage`s: level
    (log/info/warning/error/debug/trace/…), rendered arguments, source
    URL/line/column; Juggler's internal `warn` is normalized to
    `warning` (Playwright parity)
22. **Crash/close observability** — `page.is_crashed()` /
    `page.wait_for_crash(timeout)` surface `Page.crashed` (tab crashes),
    and `page.wait_for_close(timeout)` waits for target detachment
    (`Browser.detachedFromTarget`)
23. **Window management** — `page.set_viewport_size(w, h)` /
    `page.reset_viewport()` (`Page.setViewportSize`), `page.set_zoom(z)`
    (`Page.setZoom`, validated against the browser's 0.3–5.0 range) and
    `page.bring_to_front()` (`Page.bringToFront`)
24. **Page bindings** — `page.add_binding(name)` exposes
    `window.<name>` into every execution context (`Page.addBinding`);
    invocations stream through `page.binding_calls()` as `BindingCall`s
    (name + first-argument payload); `add_binding_with_script` supports
    custom worlds and setup scripts for promise-based wrappers
25. **Context permissions** — `browser.grant_permissions(ctx, origin,
    &[Permission])` grants Firefox's supported set (geolocation,
    desktop notifications, local/loopback network) per origin or `'*'`
    (`Browser.grantPermissions`); `browser.reset_permissions(ctx)`
    clears them
26. **Extra HTTP headers & cache control** — context-level
    `browser.set_extra_http_headers(ctx, headers)` and page-level
    `page.set_extra_http_headers(headers)`
    (`Browser.setExtraHTTPHeaders` / `Network.setExtraHTTPHeaders`),
    plus `browser.clear_cache()` and `browser.set_cache_disabled(ctx,
    disabled)`

Ideas for future releases:

- Drag & drop dispatch (`Page.dispatchDragEvent`)
- HTTP basic auth credentials (`Browser.setHTTPCredentials`)
- Online/offline emulation (`Browser.setOnlineOverride`)
