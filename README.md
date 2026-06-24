# rdm — Rust Download Manager

A high-performance, multi-connection HTTP/HTTPS download manager written in Rust. `rdm` is a ground-up rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm) from .NET/C# to Rust.

The project is structured as a Cargo workspace with four crates and companion browser extensions:

| Component | Binary | Description |
|-----------|--------|-------------|
| `rdm_core` | — | Core download engine (library) |
| `rdm_cli` | `rdm` | Command-line download tool |
| `rdm_server` | `rdmd` | Local HTTP daemon for browser extension integration |
| `rdm_ui` | `rdm_ui` | Dioxus desktop UI (dashboard + save dialog + progress view) |
| `rdm-chrome-extension` | — | Chrome/Chromium MV3 browser extension |
| `rdm-firefox-extension` | — | Firefox MV3 browser extension |

---

## Features

- **Parallel downloads** — splits files into up to 8 concurrent segments using HTTP `Range` requests
- **Smart segment splitting** — XDM-style dynamic binary halving (minimum segment size: 256 KB)
- **Server probing** — detects file size, resumability, filename from `Content-Disposition`, content type, `Last-Modified`, and final URL after redirects before downloading
- **Graceful fallback** — falls back to a single-connection download when the server does not support range requests
- **Retry with backoff** — automatically retries failed segments with exponential backoff (up to 3 retries: 100 ms → 200 ms → 400 ms)
- **Cancellation support** — cooperative cancellation via `CancellationToken`
- **Real-time progress** — EMA-smoothed speed, per-segment and aggregate progress with bytes downloaded, speed, and ETA
- **Persistent download dashboard** — SQLite-backed download history with stop, resume, delete-entry, and delete-entry-plus-files actions
- **Browser extension integration** — the `rdmd` daemon receives media and download events from the browser extension, spawns the `rdm_ui` desktop window for save-location selection, and streams back real-time progress via Server-Sent Events (SSE)
- **Streaming media detection** — the browser extension monitors `webRequest` traffic and posts detected audio/video URLs to `rdmd`
- **Streaming manifest downloads** — HLS (`.m3u8`) and DASH (`.mpd`) VOD manifests are routed through a dedicated streaming downloader instead of the byte-range file path
- **Download interception** — the extension cancels browser-native downloads for configured file types and hands them off to `rdmd`

---

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) (edition 2021)

### Build from source

```bash
git clone https://github.com/your-username/rdm.git
cd rdm
cargo build --release
```

Binaries will be placed at:

```
./target/release/rdm      # CLI download tool
./target/release/rdmd     # Browser extension daemon
./target/release/rdm_ui   # Desktop save/progress UI
```

> **Note:** `rdmd` discovers `rdm_ui` by looking in the same directory as itself first, then falling back to `PATH`. For the extension flow to work, both binaries must be in the same directory or `rdm_ui` must be on `PATH`.

---

## CLI Usage (`rdm`)

```bash
rdm -u <URL> -o <output_file>
```

### Options

| Flag | Description |
|------|-------------|
| `-u`, `--url` | URL to download |
| `-o`, `--output` | Output file path |
| `-c`, `--connections` | Number of parallel connections (default: 8) |

### Examples

```bash
# Download a 100 MB test file with 8 connections
rdm -u https://ash-speed.hetzner.com/100MB.bin -o /tmp/test.bin

# Limit to 4 connections
rdm -u https://ash-speed.hetzner.com/100MB.bin -o /tmp/test.bin -c 4

# Run with defaults (downloads a 1 MB test file)
rdm
```

---

## Server Daemon (`rdmd`)

`rdmd` is a local HTTP server that bridges the browser extension and the download engine. It exposes a REST + SSE API compatible with the original XDM browser extension protocol.

### Starting the daemon

```bash
# Default: 127.0.0.1:8597, 8 connections, ~/Downloads/rdm
rdmd

# Override via environment variables
RDM_HOST=127.0.0.1 RDM_PORT=8597 RDM_CONN_SIZE=8 RDM_DOWNLOAD_DIR=/tmp/rdm rdmd

# Override via CLI flags
rdmd --host 127.0.0.1 --port 8597 --connections 8
```

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RDM_HOST` | `127.0.0.1` | Bind host |
| `RDM_PORT` | `8597` | Bind port |
| `RDM_CONN_SIZE` | `8` | Max parallel connections per download |
| `RDM_DOWNLOAD_DIR` | `~/Downloads/rdm` | Directory for completed downloads |

### API endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/sync` | Heartbeat — returns server config to the extension |
| `POST` | `/download` | Start a download (called by `rdm_ui` after save-location is chosen) |
| `GET` | `/downloads` | List persisted downloads for the desktop dashboard |
| `GET` | `/downloads/{id}` | Get a single persisted download summary |
| `POST` | `/downloads/{id}/stop` | Stop a running download and keep resumable temp data |
| `POST` | `/downloads/{id}/resume` | Resume a stopped or interrupted download |
| `DELETE` | `/downloads/{id}` | Delete only the dashboard/history entry |
| `DELETE` | `/downloads/{id}/files` | Delete the history entry and local download data |
| `POST` | `/media` | Report a detected media URL |
| `POST` | `/vid` | User clicked a video in the popup — spawns `rdm_ui` |
| `POST` | `/tab-update` | Report a tab navigation event |
| `POST` | `/clear` | Clear the video list |
| `GET` | `/status/{id}` | Get the current status for a download |
| `GET` | `/progress/{id}` | SSE stream of `ProgressSnapshot` events |
| `POST` | `/cancel/{id}` | Cancel a running download |
| `GET` | `/videos` | List detected streaming media |

---

## Desktop UI (`rdm_ui`)

`rdm_ui` now serves two roles:

- launched directly, it opens the persistent download dashboard
- spawned by `rdmd`, it opens the save dialog and per-download progress window for browser-triggered downloads

### Flow

```
Browser extension popup
  → POST /vid to rdmd
    → rdmd spawns rdm_ui (VideoItem JSON sent via stdin pipe)
      → View 1: user picks save location, clicks Download
        → POST /download to rdmd
          → View 2: real-time progress bar via GET /progress/{id} SSE
            → Download complete → Close
```

### Dashboard mode

Run the binary directly to open the desktop dashboard:

```bash
./target/debug/rdm_ui
```

The dashboard reads persisted downloads from `rdmd`, shows current status/progress, and exposes **Stop**, **Resume**, **Delete Entry**, and **Delete Files** actions.

### Testing in isolation

```bash
# 1. Start rdmd
cargo run --bin rdmd

# 2. Register a test video
curl -s -X POST http://127.0.0.1:8597/videos/test123 \
  -H 'Content-Type: application/json' \
  -d '{"id":"test123","text":"Test Video","info":"video/mp4","tabId":"1",
       "url":"https://ash-speed.hetzner.com/100MB.bin",
       "cookie":"","requestHeaders":{},"responseHeaders":{}}'

# 3. Trigger the UI (same as clicking in the extension popup)
curl -s -X POST http://127.0.0.1:8597/vid \
  -H 'Content-Type: application/json' \
  -d '{"vid":"test123"}'
```

Or pipe a VideoItem directly to the binary:

```bash
echo '{"id":"test","text":"My Video","info":"video/mp4","tabId":"1",
       "url":"https://ash-speed.hetzner.com/100MB.bin",
       "cookie":"","requestHeaders":{},"responseHeaders":{}}' \
  | ./target/debug/rdm_ui
```

---

## Browser Extensions

The browser extensions intercept downloads and detected media and hand them off to `rdmd`. They are compatible with the original XDM extension protocol.

### Chrome / Chromium

1. Open `chrome://extensions`
2. Enable **Developer mode**
3. Click **Load unpacked** and select the `rdm-chrome-extension/` directory

### Firefox

1. Open `about:debugging#/runtime/this-firefox`
2. Click **Load Temporary Add-on**
3. Select `rdm-firefox-extension/manifest.json`

### What the extension does

- Intercepts browser downloads for configured file extensions and delegates them to `rdmd`
- Monitors all HTTP traffic and detects audio/video media URLs via content type and URL pattern matching
- Streams real-time download progress in the extension popup via SSE
- Provides a **"Download with rdm"** right-click context menu item
- Keeps a live list of detected streaming media for manual triggering

### Streaming support notes

- HLS (`.m3u8`) and DASH (`.mpd`) are downloaded through a separate streaming engine in `rdm_core`
- Current support targets **VOD/static manifests**
- HLS AES-128 segment encryption is supported
- Live streams, DRM-protected manifests, and DASH manifests that require separate audio/video muxing are not supported yet

> **Note:** `rdmd` must be running before the extension can intercept any downloads.

---

## Benchmarking

Compare `rdm` against `curl`, `wget`, and `aria2c`:

```bash
# Run full benchmark suite (default: 100 MB, 3 iterations)
./benchmark.sh

# Options
./benchmark.sh --size 10          # File size: 1, 10, 100, or 1000 MB
./benchmark.sh --iterations 5     # Number of iterations
./benchmark.sh --url <URL>        # Custom URL
./benchmark.sh --connections 8    # Max parallel connections (default: 8)
```

Results are saved to `./benchmark_results.csv`.

Quick single-tool test:

```bash
./quick-test.sh rdm
./quick-test.sh curl https://example.com/file.bin
./quick-test.sh aria2c-8   # aria2c with 8 connections
```

---

## Development

```bash
# Run all tests
cargo test

# Debug build
cargo build

# Release build (optimised, stripped)
cargo build --release
```

### Project structure

```
rdm/
├── Cargo.toml                  # Workspace root
├── rdm_core/                   # Core download engine (library crate)
│   └── src/
│       ├── downloader/         # HttpDownloader, segment_grabber, strategies
│       ├── progress/           # ProgressObserver trait, notifier, snapshots
│       └── types/              # Shared types and errors
├── rdm_cli/                    # CLI binary (rdm)
│   └── src/
│       ├── main.rs
│       └── terminal_observer.rs
├── rdm_server/                 # Server daemon (rdmd)
│   └── src/
│       ├── server.rs           # Axum router and all HTTP handlers
│       ├── sse_observer.rs     # SSE progress push
│       ├── video_tracker.rs    # In-memory detected media list
│       └── path_sanitizer.rs   # Safe output path generation
├── rdm_ui/                     # Desktop UI binary (rdm_ui)
│   ├── assets/
│   │   └── app.css             # All UI styles (embedded at compile time)
│   └── src/
│       ├── main.rs             # Entry point — reads VideoItem from stdin, launches window
│       ├── app.rs              # Dioxus components: App, FilePickerView, ProgressView
│       ├── api.rs              # HTTP client — trigger_download, cancel_download, subscribe_progress
│       └── styles.rs           # include_str! embed of assets/app.css
├── rdm-chrome-extension/       # Chrome MV3 extension
└── rdm-firefox-extension/      # Firefox MV3 extension
```

---

## Roadmap

- [x] Phase 1 — Core CLI download engine (multi-part, retry, cancellation)
- [x] Phase 2 — Browser extension integration (local HTTP daemon, SSE, Chrome + Firefox)
- [x] Phase 3 — Desktop UI (Dioxus desktop, save-location picker, real-time progress view)
- [ ] Phase 4 — Dual-source downloads, HLS/DASH streaming, FFmpeg support
- [ ] Phase 5 — SQLite persistence, download history, resume state
- [ ] Phase 6 — Clipboard monitoring, system tray, browser context menus
- [ ] Phase 7 — Regression and stress testing
- [ ] Phase 8 — Packaging (MSI, .deb, .rpm, DMG, Homebrew)

---

## License

This project is a spiritual rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm). Please refer to the original project for licensing context.
