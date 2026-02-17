#!/usr/bin/env bash
#
# benchmark.sh — Compare rdm against curl, wget, and aria2c
#
# Usage:
#   ./benchmark.sh                   # Run with defaults (100MB file, 3 iterations)
#   ./benchmark.sh --size 10         # Use 10MB test file
#   ./benchmark.sh --iterations 5    # Run 5 iterations per tool
#   ./benchmark.sh --url <url>       # Use a custom URL
#
# Requirements: curl, wget, aria2c, and rdm (built in release mode)

set -euo pipefail

# ──────────────────────────────────────────────────────────────
# Configuration
# ──────────────────────────────────────────────────────────────

ITERATIONS=3
FILE_SIZE_MB=100
CUSTOM_URL=""
RDM_BIN="./target/release/rdm"
BENCH_DIR="/tmp/rdm_benchmark_$$"
CONNECTIONS=8
RESULTS_FILE=""

# Tool names (order matters for display)
TOOL_NAMES=""  # populated later

# ANSI colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ──────────────────────────────────────────────────────────────
# Parse arguments
# ──────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case $1 in
        --size)
            FILE_SIZE_MB="$2"
            shift 2
            ;;
        --iterations)
            ITERATIONS="$2"
            shift 2
            ;;
        --url)
            CUSTOM_URL="$2"
            shift 2
            ;;
        --connections)
            CONNECTIONS="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --size <MB>         File size to test: 1, 10, 100, 1000 (default: 100)"
            echo "  --iterations <N>    Number of iterations per tool (default: 3)"
            echo "  --url <URL>         Custom download URL (overrides --size)"
            echo "  --connections <N>   Max connections for multi-connection tools (default: 8)"
            echo "  -h, --help          Show this help"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# ──────────────────────────────────────────────────────────────
# Determine test URL
# ──────────────────────────────────────────────────────────────

if [[ -n "$CUSTOM_URL" ]]; then
    TEST_URL="$CUSTOM_URL"
    FILE_DESC="custom URL"
else
    # Hetzner speed test files (fast, reliable, support Range requests)
    # Using Ashburn, VA datacenter (ash-speed)
    case "$FILE_SIZE_MB" in
        1)
            # Hetzner doesn't have 1MB, use 100MB
            echo "Note: Hetzner doesn't have 1MB test file, using 100MB instead"
            TEST_URL="https://ash-speed.hetzner.com/100MB.bin"
            FILE_DESC="100MB"
            FILE_SIZE_MB=100
            ;;
        10)
            # Hetzner doesn't have 10MB, use 100MB
            echo "Note: Hetzner doesn't have 10MB test file, using 100MB instead"
            TEST_URL="https://ash-speed.hetzner.com/100MB.bin"
            FILE_DESC="100MB"
            FILE_SIZE_MB=100
            ;;
        100)  
            TEST_URL="https://ash-speed.hetzner.com/100MB.bin"
            FILE_DESC="100MB"
            ;;
        1000) 
            TEST_URL="https://ash-speed.hetzner.com/1GB.bin"
            FILE_DESC="1GB"
            ;;
        *)
            echo "Unsupported size: ${FILE_SIZE_MB}MB. Use 1, 10, 100, or 1000."
            exit 1
            ;;
    esac
fi

# Build tool names list (aria2c name depends on CONNECTIONS)
ARIA2_MULTI="aria2c_${CONNECTIONS}conn"
TOOL_NAMES="curl wget aria2c_1conn ${ARIA2_MULTI} rdm"

# ──────────────────────────────────────────────────────────────
# Preflight checks
# ──────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}${CYAN}║          RDM Download Manager Benchmark Suite               ║${NC}"
echo -e "${BOLD}${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
echo ""

for tool in curl wget aria2c; do
    if ! command -v "$tool" &>/dev/null; then
        echo -e "${RED}[MISSING]${NC} $tool not found in PATH"
    else
        version=$("$tool" --version 2>/dev/null | head -1)
        echo -e "${GREEN}[OK]${NC} $tool: $version"
    fi
done

if [[ ! -x "$RDM_BIN" ]]; then
    echo -e "${YELLOW}[BUILD]${NC} rdm release binary not found, building..."
    cargo build --release 2>&1
    if [[ ! -x "$RDM_BIN" ]]; then
        echo -e "${RED}[ERROR]${NC} Failed to build rdm"
        exit 1
    fi
fi
echo -e "${GREEN}[OK]${NC} rdm: $RDM_BIN"

echo ""
echo -e "${BOLD}Configuration:${NC}"
echo "  File:         $FILE_DESC"
echo "  URL:          $TEST_URL"
echo "  Iterations:   $ITERATIONS"
echo "  Connections:  $CONNECTIONS (for aria2c & rdm)"
echo "  Temp dir:     $BENCH_DIR"
echo ""

mkdir -p "$BENCH_DIR"
RESULTS_FILE="$BENCH_DIR/results.csv"
echo "tool,iteration,time_seconds,file_size_bytes,speed_mbps" > "$RESULTS_FILE"

# Per-tool times are stored in flat files for bash 3.2 compat
TIMES_DIR="$BENCH_DIR/times"
mkdir -p "$TIMES_DIR"

# ──────────────────────────────────────────────────────────────
# Utility functions
# ──────────────────────────────────────────────────────────────

cleanup_download() {
    rm -f "$BENCH_DIR"/download_* "$BENCH_DIR"/*.aria2 "$BENCH_DIR"/*.tmp 2>/dev/null || true
}

warmup() {
    echo -e "${YELLOW}Warming up (DNS + TLS)...${NC}"
    curl -sS -o /dev/null -r 0-1023 "$TEST_URL" 2>/dev/null || true
    echo ""
}

# High-resolution timer using python3
time_download() {
    local start end elapsed
    start=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    "$@" >/dev/null 2>&1
    local exit_code=$?
    end=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    elapsed=$(python3 -c "print(f'{$end - $start:.4f}')")
    echo "$elapsed"
    return $exit_code
}

compute_stats() {
    python3 -c "
import sys
times = [float(x) for x in sys.argv[1:]]
n = len(times)
avg = sum(times) / n
times_sorted = sorted(times)
median = times_sorted[n // 2] if n % 2 else (times_sorted[n//2 - 1] + times_sorted[n//2]) / 2
mn = min(times)
mx = max(times)
if n > 1:
    variance = sum((t - avg) ** 2 for t in times) / (n - 1)
    stddev = variance ** 0.5
else:
    stddev = 0.0
print(f'{avg:.4f} {median:.4f} {mn:.4f} {mx:.4f} {stddev:.4f}')
" "$@"
}

# ──────────────────────────────────────────────────────────────
# Benchmark runner
# ──────────────────────────────────────────────────────────────

run_benchmark() {
    local tool_name="$1"
    shift
    local cmd_desc="$1"
    shift

    echo -e "${BOLD}${BLUE}── $tool_name ──${NC}"
    echo -e "  Command: ${cmd_desc}"

    # Clear times file
    > "$TIMES_DIR/${tool_name}.times"

    local i=1
    while [[ $i -le $ITERATIONS ]]; do
        cleanup_download

        local elapsed
        elapsed=$(time_download "$@") || true

        # Get downloaded file size
        local out_file="$BENCH_DIR/download_${tool_name}"
        local file_size=0
        if [[ -f "$out_file" ]]; then
            file_size=$(wc -c < "$out_file" | tr -d ' ')
        fi

        # If file is empty or doesn't exist, check if aria2 used its own naming
        if [[ "$file_size" -eq 0 ]]; then
            # aria2c writes to -d/-o path
            local aria2_file="$BENCH_DIR/${tool_name##download_}"
            if [[ -f "$aria2_file" ]]; then
                file_size=$(wc -c < "$aria2_file" | tr -d ' ')
            fi
        fi

        local speed_mbps
        speed_mbps=$(python3 -c "
sz = $file_size; t = $elapsed
print(f'{(sz * 8) / (t * 1000000):.2f}' if t > 0 else '0.00')
")

        echo -e "  Run $i: ${GREEN}${elapsed}s${NC}  (${speed_mbps} Mbps, ${file_size} bytes)"

        # Store time
        echo "$elapsed" >> "$TIMES_DIR/${tool_name}.times"

        # Store in CSV
        echo "${tool_name},${i},${elapsed},${file_size},${speed_mbps}" >> "$RESULTS_FILE"

        i=$((i + 1))
    done

    cleanup_download
    echo ""
}

# ──────────────────────────────────────────────────────────────
# Run all benchmarks
# ──────────────────────────────────────────────────────────────

warmup

echo -e "${BOLD}${CYAN}Starting benchmarks...${NC}"
echo -e "${BOLD}${CYAN}════════════════════════════════════════════════════════════${NC}"
echo ""

# 1. curl (single connection — baseline)
run_benchmark "curl" \
    "curl -sS -o <output> <url>" \
    curl -sS -o "$BENCH_DIR/download_curl" "$TEST_URL"

# 2. wget (single connection)
run_benchmark "wget" \
    "wget -q -O <output> <url>" \
    wget -q -O "$BENCH_DIR/download_wget" "$TEST_URL"

# 3. aria2c single connection (fair comparison)
run_benchmark "aria2c_1conn" \
    "aria2c -x1 -s1 -q -d <dir> -o <output> <url>" \
    aria2c -x1 -s1 --allow-overwrite=true -q \
    -d "$BENCH_DIR" -o "download_aria2c_1conn" "$TEST_URL"

# 4. aria2c multi-connection
run_benchmark "${ARIA2_MULTI}" \
    "aria2c -x${CONNECTIONS} -s${CONNECTIONS} -q -d <dir> -o <output> <url>" \
    aria2c "-x${CONNECTIONS}" "-s${CONNECTIONS}" --allow-overwrite=true -q \
    -d "$BENCH_DIR" -o "download_${ARIA2_MULTI}" "$TEST_URL"

# 5. rdm (multi-connection, 8 connections by default)
run_benchmark "rdm" \
    "rdm -u <url> -o <output>" \
    "$RDM_BIN" -u "$TEST_URL" -o "$BENCH_DIR/download_rdm"

# ──────────────────────────────────────────────────────────────
# Results summary — computed entirely in Python for reliability
# ──────────────────────────────────────────────────────────────

echo -e "${BOLD}${CYAN}════════════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}${CYAN}                        RESULTS SUMMARY                         ${NC}"
echo -e "${BOLD}${CYAN}════════════════════════════════════════════════════════════════${NC}"
echo ""

python3 - "$TIMES_DIR" "$RESULTS_FILE" "$TOOL_NAMES" <<'PYEOF'
import sys, os, math

times_dir = sys.argv[1]
results_csv = sys.argv[2]
tool_names = sys.argv[3].split()

# Colors
GREEN = "\033[0;32m"
RED = "\033[0;31m"
BOLD = "\033[1m"
NC = "\033[0m"

# Load per-tool times
tool_data = {}
for tool in tool_names:
    times_file = os.path.join(times_dir, f"{tool}.times")
    if os.path.exists(times_file):
        with open(times_file) as f:
            times = [float(line.strip()) for line in f if line.strip()]
        if times:
            tool_data[tool] = times

# Load file sizes from CSV
tool_file_size = {}
with open(results_csv) as f:
    next(f)  # skip header
    for line in f:
        parts = line.strip().split(",")
        if len(parts) >= 4:
            tool_file_size[parts[0]] = int(parts[3])

# Compute stats
def stats(times):
    n = len(times)
    avg = sum(times) / n
    s = sorted(times)
    median = s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2
    mn, mx = min(times), max(times)
    stddev = (sum((t - avg) ** 2 for t in times) / (n - 1)) ** 0.5 if n > 1 else 0
    return avg, median, mn, mx, stddev

# Find fastest
tool_stats = {}
fastest_avg = float("inf")
fastest_tool = ""
for tool in tool_names:
    if tool in tool_data:
        s = stats(tool_data[tool])
        tool_stats[tool] = s
        if s[0] < fastest_avg:
            fastest_avg = s[0]
            fastest_tool = tool

# Print table
header = f"{'Tool':<20s} {'Avg (s)':>10s} {'Median':>10s} {'Min':>10s} {'Max':>10s} {'StdDev':>10s}"
print(f"{BOLD}{header}{NC}")
print(f"{'─' * 20} {'─' * 10} {'─' * 10} {'─' * 10} {'─' * 10} {'─' * 10}")

for tool in tool_names:
    if tool in tool_stats:
        avg, median, mn, mx, stddev = tool_stats[tool]
        row = f"{tool:<20s} {avg:>10.4f} {median:>10.4f} {mn:>10.4f} {mx:>10.4f} {stddev:>10.4f}"
        if tool == fastest_tool:
            print(f"{GREEN}{BOLD}{row}  << fastest{NC}")
        else:
            pct = ((avg / fastest_avg) - 1) * 100
            print(f"{row}  (+{pct:.1f}%)")
    else:
        print(f"{RED}{tool:<20s} {'FAILED':>10s}{NC}")

print()

# Speed comparison
print(f"{BOLD}Speed comparison (average throughput):{NC}")
print(f"{'─' * 20} {'─' * 16}")
for tool in tool_names:
    if tool in tool_stats:
        avg = tool_stats[tool][0]
        fsize = tool_file_size.get(tool, 0)
        if fsize > 0 and avg > 0:
            mbps = (fsize * 8) / (avg * 1_000_000)
            mb_s = fsize / (avg * 1_048_576)
            print(f"{tool:<20s} {mbps:>8.2f} Mbps  ({mb_s:.2f} MB/s)")

print()

# Bar chart
print(f"{BOLD}Relative performance (shorter = faster):{NC}")
print()
max_avg = max(s[0] for s in tool_stats.values()) if tool_stats else 1
bar_width = 50
for tool in tool_names:
    if tool in tool_stats:
        avg = tool_stats[tool][0]
        bar_len = int((avg / max_avg) * bar_width)
        bar_len = max(bar_len, 1)
        bar = "█" * bar_len
        if tool == fastest_tool:
            print(f"  {tool:<20s} {GREEN}{bar}{NC} {avg:.4f}s")
        else:
            print(f"  {tool:<20s} {bar} {avg:.4f}s")

print()
PYEOF

# Copy results to project root before cleanup
cp "$RESULTS_FILE" ./benchmark_results.csv 2>/dev/null || true

echo -e "Raw CSV saved to: ${CYAN}./benchmark_results.csv${NC}"
echo ""

# Cleanup
echo -e "${YELLOW}Cleaning up temp files...${NC}"
rm -rf "$BENCH_DIR"
echo -e "${GREEN}Done.${NC}"
