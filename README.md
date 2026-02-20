# rdm — Rust Download Manager

A high-performance, multi-connection HTTP/HTTPS download manager written in Rust. `rdm` is a ground-up rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm) from .NET/C# to Rust, starting with the core download engine.

## Features

- **Parallel downloads** — splits files into up to 8 concurrent pieces using `Range` requests
- **Smart piece splitting** — XDM-style dynamic binary halving (minimum piece size: 256 KB)
- **Server probing** — detects file size, resumability, filename from `Content-Disposition`, content type, and `Last-Modified` header before downloading
- **Graceful fallback** — falls back to single-connection download when the server doesn't support range requests
- **Retry with backoff** — automatically retries failed pieces with exponential backoff (up to 3 retries: 100ms → 200ms → 400ms)
- **Cancellation support** — cooperative cancellation via `CancellationToken`
- **Real-time progress** — progress events emitted via async mpsc channel
- **Custom headers, cookies, Basic auth, and proxy** support

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) (edition 2021)

### Build from source

```bash
git clone https://github.com/your-username/rdm.git
cd rdm
cargo build --release
```

The binary will be at `./target/release/rdm`.

## Usage

```bash
rdm -u <URL> -o <output_file>
```

### Examples

```bash
# Download a 100MB test file
rdm -u https://ash-speed.hetzner.com/100MB.bin -o /tmp/test.bin

# Default (uses a 1MB test file)
rdm
```

### Options

| Flag | Description |
|------|-------------|
| `-u`, `--url` | URL to download |
| `-o`, `--output` | Output file path |

## Benchmarking

Compare `rdm` against `curl`, `wget`, and `aria2c`:

```bash
# Run full benchmark suite (default: 100MB, 3 iterations)
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

## Development

```bash
# Run tests
cargo test

# Debug build
cargo build
```

## Roadmap

- [x] Phase 1 — Core CLI download engine (multi-part, retry, cancellation)
- [ ] Phase 2 — Browser extension integration (native messaging, IPC)
- [ ] Phase 3 — Dual-source downloads, HLS/DASH streaming, FFmpeg support
- [ ] Phase 4 — SQLite persistence, download history, resume state
- [ ] Phase 5 — GUI (Dioxus Desktop)
- [ ] Phase 6 — Clipboard monitoring, system tray, browser context menus
- [ ] Phase 7 — Regression and stress testing
- [ ] Phase 8 — Packaging (MSI, .deb, .rpm, DMG, Homebrew)

## License

This project is a spiritual rewrite of [XDM (Xtreme Download Manager)](https://github.com/subhra74/xdm). Please refer to the original project for licensing context.
