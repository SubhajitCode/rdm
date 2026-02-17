#!/usr/bin/env bash
#
# Quick speed test for a single tool
# Usage: ./quick-test.sh [tool] [url]
#   tool: rdm, curl, wget, aria2c, aria2c-8
#   url: optional (defaults to Hetzner 100MB)

set -euo pipefail

TOOL="${1:-rdm}"
URL="${2:-https://ash-speed.hetzner.com/100MB.bin}"
OUTPUT="/tmp/speed_test_$$"

echo "Testing: $TOOL"
echo "URL: $URL"
echo ""

# High-resolution timer
start=$(python3 -c 'import time; print(f"{time.time():.6f}")')

case "$TOOL" in
    rdm)
        ./target/release/rdm -u "$URL" -o "$OUTPUT"
        ;;
    curl)
        curl -sS -o "$OUTPUT" "$URL"
        ;;
    wget)
        wget -q -O "$OUTPUT" "$URL"
        ;;
    aria2c|aria2c-1)
        aria2c -x1 -s1 -q -d /tmp -o "speed_test_$$" "$URL"
        ;;
    aria2c-8)
        aria2c -x8 -s8 -q -d /tmp -o "speed_test_$$" "$URL"
        ;;
    *)
        echo "Unknown tool: $TOOL"
        echo "Available: rdm, curl, wget, aria2c, aria2c-8"
        exit 1
        ;;
esac

end=$(python3 -c 'import time; print(f"{time.time():.6f}")')
elapsed=$(python3 -c "print(f'{$end - $start:.4f}')")

# Get file size
if [[ -f "$OUTPUT" ]]; then
    size=$(wc -c < "$OUTPUT" | tr -d ' ')
elif [[ -f "/tmp/speed_test_$$" ]]; then
    size=$(wc -c < "/tmp/speed_test_$$" | tr -d ' ')
else
    echo "Error: Output file not found"
    exit 1
fi

# Calculate speed
mbps=$(python3 -c "print(f'{($size * 8) / ($elapsed * 1000000):.2f}')")
mbs=$(python3 -c "print(f'{$size / ($elapsed * 1048576):.2f}')")

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Time:     ${elapsed}s"
echo "  Size:     $size bytes ($(numfmt --to=iec-i --suffix=B $size 2>/dev/null || echo "$size bytes"))"
echo "  Speed:    ${mbps} Mbps (${mbs} MB/s)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Cleanup
rm -f "$OUTPUT" "/tmp/speed_test_$$"
