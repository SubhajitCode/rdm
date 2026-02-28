# rdm — Rust Download Manager

A high-performance, multi-connection HTTP/HTTPS download manager written in Rust. `rdm` is a ground-up rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm) from .NET/C# to Rust.

The project is structured as a Cargo workspace with three crates and companion browser extensions:

| Component | Binary | Description |
|-----------|--------|-------------|
| `rdm_core` | — | Core download engine (library) |
| `rdm_cli` | `rdm` | Command-line download tool |
| `rdm_server` | `rdmd` | Local HTTP daemon for browser extension integration |
| `rdm-chrome-extension` | — | Chrome/Chromium MV3 browser extension |
| `rdm-firefox-extension` | — | Firefox MV3 browser extension |

---

## Features

- **Parallel downloads** — splits files into up to 8 concurrent pieces using HTTP `Range` requests
- **Smart piece splitting** — XDM-style dynamic binary halving (minimum piece size: 256 KB)
- **Server probing** — detects file size, resumability, filename from `Content-Disposition`, content type, `Last-Modified`, and final URL after redirects before downloading
- **Graceful fallback** — falls back to a single-connection download when the server does not support range requests
- **Retry with backoff** — automatically retries failed pieces with exponential backoff (up to 3 retries: 100 ms → 200 ms → 400 ms)
- **Cancellation support** — cooperative cancellation via `CancellationToken`
- **Real-time progress** — EMA-smoothed speed, per-piece and aggregate progress with bytes downloaded, speed, and ETA
- **Browser extension integration** — the `rdmd` daemon receives media and download events from the browser extension, triggers downloads, and streams back progress via Server-Sent Events (SSE)
- **Streaming media detection** — the browser extension monitors `webRequest` traffic and posts detected audio/video URLs to `rdmd`
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
./target/release/rdm     # CLI download tool
./target/release/rdmd    # Browser extension daemon
```

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
| `POST` | `/download` | Start a new download |
| `POST` | `/media` | Report a detected media URL |
| `POST` | `/vid` | Report a detected video stream |
| `POST` | `/tab-update` | Report a tab navigation event |
| `POST` | `/clear` | Clear the video list |
| `GET` | `/status/{id}` | Get the current `ProgressSnapshot` for a download |
| `GET` | `/progress/{id}` | SSE stream of progress events for a download |
| `POST` | `/cancel/{id}` | Cancel a running download |
| `GET` | `/videos` | List detected streaming media |

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
│       ├── downloader/         # HttpDownloader, piece_grabber, strategies
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
│       └── path_sanitizer.rs  # Safe output path generation
├── rdm-chrome-extension/       # Chrome MV3 extension
└── rdm-firefox-extension/      # Firefox MV3 extension
```

---

## Roadmap

- [x] Phase 1 — Core CLI download engine (multi-part, retry, cancellation)
- [x] Phase 2 — Browser extension integration (local HTTP daemon, SSE, Chrome + Firefox)
- [ ] Phase 3 — Dual-source downloads, HLS/DASH streaming, FFmpeg support
- [ ] Phase 4 — SQLite persistence, download history, resume state
- [ ] Phase 5 — GUI (Dioxus Desktop)
- [ ] Phase 6 — Clipboard monitoring, system tray, browser context menus
- [ ] Phase 7 — Regression and stress testing
- [ ] Phase 8 — Packaging (MSI, .deb, .rpm, DMG, Homebrew)

---

## License

This project is a spiritual rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm). Please refer to the original project for licensing context.
