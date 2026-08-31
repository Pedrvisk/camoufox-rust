# Plano de Migração: camoufox-js → camoufox-rust

Port completo do projeto `camoufox-js-master` (TypeScript/Node.js) para Rust, com
arquitetura em camadas de domínio (blocos reutilizáveis bem definidos), mantendo
paridade de comportamento com o original.

---

## 1. Análise do projeto original

| Módulo JS | Linhas | Responsabilidade |
|---|---|---|
| `src/utils.ts` | 867 | `LaunchOptions`, montagem do config (fingerprint, fonts, geoip, webgl, seeds), validação, env vars `CAMOU_CONFIG_*` |
| `src/pkgman.ts` | 520 | Download/instalação do browser via GitHub Releases, versionamento, caminhos, `webdl` |
| `src/locale.ts` | 406 | Seleção estatística de locale (territoryInfo.xml), geolocalização (MaxMind), download do mmdb |
| `src/exceptions.ts` | 182 | Hierarquia de exceções |
| `src/fingerprints.ts` | 120 | Geração de fingerprint (fingerprint-generator) + conversão p/ config Camoufox |
| `src/addons.ts` | 101 | Addons padrão (uBlock Origin): download, extração, validação |
| `src/virtdisplay.ts` | 168 | Xvfb com `-displayfd 3` (Linux) |
| `src/webgl/sample.ts` | 158 | Amostragem ponderada de fingerprints WebGL (sqlite) |
| `src/__main__.ts` | 162 | CLI (fetch/remove/test/server/path/version) |
| `src/sync_api.ts` + `server.ts` + `index.ts` | 106 | Fachadas Playwright |
| `src/mappings/*` | 952 | Tabelas: browserforge.config, fonts.config, warnings.config |
| `src/ip.ts` | 142 | IP público, validação IPv4/IPv6, helpers de proxy |
| **Total** | ~3942 | |

## 2. Arquitetura Rust (workspace com 6 crates)

Domain-driven + hexagonal leve: **core puro** (sem IO), **infraestrutura** (IO/adapters)
e **fachada de aplicação** (orquestração + CLI).

```
camoufox-rust/
├── crates/
│   ├── camoufox-core          # DOMÍNIO PURO — sem rede, sem filesystem
│   │   ├── error.rs           # CamoufoxError (todas as exceções portadas)
│   │   ├── os.rs              # HostOs / OsName (win|mac|lin) / SupportedOs
│   │   ├── env_utils.rs       # getAsBooleanFromENV (PLAYWRIGHT_SKIP_*)
│   │   ├── mappings/          # browserforge.rs, fonts.rs (gerado), warnings.rs
│   │   ├── fingerprint.rs     # geração (veilus-fingerprint) + from_browserforge
│   │   ├── config.rs          # ConfigMap, validação, seeds, env chunking
│   │   └── locale.rs          # Locale/Geolocation + seletor estatístico (XML embutido)
│   ├── camoufox-pkgman        # INFRA — GitHub releases, zip, versões, addons, caminhos
│   ├── camoufox-geoip         # INFRA — IP público (reqwest+proxy), MaxMind mmdb
│   ├── camoufox-webgl         # INFRA — webgl_data.db (sqlite embutido)
│   ├── camoufox-virtdisplay   # INFRA — Xvfb -displayfd (só Linux)
│   └── camoufox               # FACHADA — LaunchOptions → PreparedLaunch → launch() + CLI
└── PLAN.md / README.md
```

### Fluxo de dependência

```
camoufox (facade/CLI)
 ├── camoufox-pkgman ──► camoufox-core
 ├── camoufox-geoip ───► camoufox-core + camoufox-pkgman
 ├── camoufox-webgl ───► camoufox-core
 └── camoufox-virtdisplay
```

## 3. Decisões técnicas

| Tema | JS | Rust |
|---|---|---|
| Fingerprints | npm `fingerprint-generator` | crate **`veilus-fingerprint`** (browserforge-compatible, Bayes, `oscpu`/`screenX` presentes) |
| HTTP | `fetch` + `impit` | `reqwest` (rustls) |
| GeoIP | `maxmind` (npm) | `maxminddb` |
| SQLite | `better-sqlite3`/`bun:sqlite` | `rusqlite` (bundled) |
| XML | `xml2js` | `quick-xml` |
| Zip | `adm-zip` | `zip` |
| CLI | `commander` | `clap` (derive) |
| Progresso | `cli-progress` | `indicatif` |
| UA parsing | `ua-parser-js` | matching estrutural simples (Windows/Mac/Linux) |
| Xvfb fd3 | spawn `stdio[3]` | `pre_exec` + `dup2(pipe_w, 3)` + `tokio::io::unix::AsyncFd` |
| Erros | classes `Error` | `thiserror` (enum único com variantes equivalentes) |

### Diferenças intencionais (melhorias)

1. **`PreparedLaunch` serializável** em vez de monkeypatching do Playwright:
   `spoofs_window_dimensions` é consultado direto no `ConfigMap` (o JS remontava os
   chunks de env var para descobrir isso). Consumidores Playwright devem usar
   `viewport: null` quando `spoofs_window_dimensions() == true` (documentado).
2. **Auto-instalação esperada (awaited)**: no JS `camoufoxPath()` dispara o download
   sem aguardar (bug latente); no Rust `get_path()`/`prepare()` aguardam a instalação.
3. **Dados embutidos no binário** (`include_str!`/`include_bytes!`): territoryInfo.xml
   e webgl_data.db não dependem de caminho relativo em runtime.
4. **Constraints de tela** via rejection sampling (o `veilus-fingerprint` não tem
   opção nativa de min/max — regenera até satisfazer, com limite de tentativas).
5. **`launch()` direto**: além de produzir o `PreparedLaunch` (integrável com
   qualquer driver Playwright), a fachada sobe o processo do browser com env/prefs
   (`user.js`) sem depender do Playwright.
6. `launchServer` (Playwright BrowserServer) não existe em Rust — substituído pelo
   comando `camoufox prepare --json` que expõe tudo que um driver externo precisa.

## 4. Paridade módulo → módulo

- [x] `exceptions.ts` → `camoufox-core/src/error.rs`
- [x] `__version__.ts` (CONSTRAINTS alpha.1..<1) → `camoufox-pkgman/src/version.rs`
- [x] `mappings/browserforge.config.ts` → `camoufox-core/src/mappings/browserforge.rs`
- [x] `mappings/fonts.config.ts` → gerado em `camoufox-core/src/mappings/fonts.rs`
- [x] `mappings/warnings.config.ts` → `camoufox-core/src/mappings/warnings.rs`
- [x] `fingerprints.ts` → `camoufox-core/src/fingerprint.rs` (veilus-fingerprint)
- [x] `utils.ts` (config/env/validação) → `camoufox-core/src/config.rs`
- [x] `locale.ts` (parte pura) → `camoufox-core/src/locale.rs` (XML embutido)
- [x] `ip.ts` → `camoufox-geoip/src/public_ip.rs` (+ validação IpAddr em core)
- [x] `locale.ts` (mmdb) → `camoufox-geoip/src/mmdb.rs`
- [x] `pkgman.ts` → `camoufox-pkgman` (github.rs, install.rs, paths.rs, download.rs, addons.rs)
- [x] `webgl/sample.ts` → `camoufox-webgl`
- [x] `virtdisplay.ts` → `camoufox-virtdisplay`
- [x] `utils.ts` (`launchOptions`) + `sync_api.ts` → `camoufox/src/builder.rs` + `launch.rs`
- [x] `__main__.ts` → `camoufox/src/cli.rs` (fetch/remove/test/path/version/prepare)
- [x] testes unitários portados (versões, chunking, locale, mapeamento, validação, webgl, fonts)

## 5. Fases executadas

1. [x] Análise do código JS completo (16 arquivos + 6 testes)
2. [x] Scaffold do workspace + data files + fonts gerado
3. [x] camoufox-core (domínio puro)
4. [x] camoufox-pkgman / geoip / webgl / virtdisplay (infraestrutura)
5. [x] Fachada `camoufox` (LaunchOptions, PreparedLaunch, launch, CLI)
6. [x] `cargo build` + `cargo test` verdes

## 6. Riscos conhecidos

- O binário Camoufox em si (Firefox patcheado) permanece o mesmo — baixado do
  GitHub Releases `daijro/camoufox`; apenas o wrapper foi portado.
- O playbook de automação (protocolo Juggler) não tem cliente Rust maduro; a
  integração recomendada é via `PreparedLaunch` + driver Playwright externo, ou
  `launch()` p/ execução direta do browser.
