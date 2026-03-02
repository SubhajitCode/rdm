#!/usr/bin/env zsh
# test_parallel_ranges.sh
#
# Fires 8 concurrent curl requests with consecutive 10 MB ranges against a URL.
# Usage: ./test_parallel_ranges.sh <URL> [output_dir]
#
# Each segment is saved to <output_dir>/segment_N.bin
# A summary table is printed after all requests complete.

setopt errexit nounset pipefail

URL="${1:-}"
OUT_DIR="${2:-/tmp/rdm_range_test}"

if [[ -z "$URL" ]]; then
    echo "Usage: $0 <URL> [output_dir]"
    exit 1
fi

NUM_SEGMENTS=8
SEGMENT_SIZE=$(( 10 * 1024 * 1024 ))   # 10 MB in bytes

mkdir -p "$OUT_DIR"

echo "URL          : $URL"
echo "Segments     : $NUM_SEGMENTS"
echo "Segment size : $(( SEGMENT_SIZE / 1024 / 1024 )) MB each"
echo "Output dir   : $OUT_DIR"
echo ""
echo "Launching $NUM_SEGMENTS concurrent curl requests..."
echo ""

# zsh arrays are 1-indexed; we index from 1 to NUM_SEGMENTS
typeset -a PIDS

for i in $(seq 1 $NUM_SEGMENTS); do
    START=$(( (i - 1) * SEGMENT_SIZE ))
    END=$(( START + SEGMENT_SIZE - 1 ))
    OUT_FILE="$OUT_DIR/segment_${i}.bin"
    LOG_FILE="$OUT_DIR/segment_${i}.log"

    curl \
        --silent \
        --show-error \
        --range "${START}-${END}" \
        --output "$OUT_FILE" \
        --write-out "  [segment $i] status=%{http_code}  downloaded=%{size_download} B  speed=%{speed_download} B/s  time=%{time_total}s\n" \
        --dump-header "$LOG_FILE" \
        "$URL" &

    PIDS[$i]=$!
    echo "  segment $i : bytes=${START}-${END}  (pid=${PIDS[$i]})"
done

echo ""
echo "Waiting for all segments to complete..."
echo ""

# Wait for every PID and store exit codes (also 1-indexed)
typeset -a EXIT_CODES
for i in $(seq 1 $NUM_SEGMENTS); do
    wait "${PIDS[$i]}" && EXIT_CODES[$i]=0 || EXIT_CODES[$i]=$?
done

echo ""
echo "--------------------------------------------------------------------"
echo " Results"
echo "--------------------------------------------------------------------"
printf "%-10s %-8s %-12s %-16s\n" "Segment" "Exit" "HTTP Status" "Bytes on disk"
echo "--------------------------------------------------------------------"

for i in $(seq 1 $NUM_SEGMENTS); do
    OUT_FILE="$OUT_DIR/segment_${i}.bin"
    LOG_FILE="$OUT_DIR/segment_${i}.log"

    DISK_SIZE=0
    [[ -f "$OUT_FILE" ]] && DISK_SIZE=$(wc -c < "$OUT_FILE" | tr -d ' ')

    HTTP_STATUS="n/a"
    [[ -f "$LOG_FILE" ]] && HTTP_STATUS=$(head -1 "$LOG_FILE" | awk '{print $2}')

    printf "%-10s %-8s %-12s %-16s\n" \
        "$i" "${EXIT_CODES[$i]}" "$HTTP_STATUS" "${DISK_SIZE} B"
done

echo "--------------------------------------------------------------------"
echo ""
echo "Segment files : $OUT_DIR/segment_*.bin"
echo "Header dumps  : $OUT_DIR/segment_*.log"
