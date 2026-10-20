#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Spike measurement procedure for Linux and macOS (measure.ps1 is the Windows twin).
# Usage: measure.sh <path-to-spike-binary>
# Appends to $RESULTS_DIR/results.txt (default: current directory) and writes cold.json.
set -u

bin=${1:?usage: measure.sh <spike-binary>}
dir=${RESULTS_DIR:-.}
out="$dir/results.txt"
mkdir -p "$dir"

{
    echo "== date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "== host: $(uname -srm)"
    echo "== smoke"
} >>"$out"

"$bin" --smoke >"$dir/smoke.out" 2>"$dir/smoke.err"
smoke_status=$?
cat "$dir/smoke.out" >>"$out"
echo "SMOKE_EXIT=$smoke_status" >>"$out"
if [ "$smoke_status" -ne 0 ]; then
    echo "== smoke stderr (tail)" >>"$out"
    tail -n 40 "$dir/smoke.err" >>"$out"
fi

# Binary size, unstripped, default release profile.
case "$(uname -s)" in
    Darwin) bytes=$(stat -f %z "$bin") ;;
    *) bytes=$(stat -c %s "$bin") ;;
esac
mb=$(awk -v b="$bytes" 'BEGIN { printf "%.1f", b / 1048576 }')
echo "BINARY_BYTES=$bytes" >>"$out"
echo "BINARY_MB=$mb" >>"$out"

# Cold start: process start to exit after the first presented frame.
if hyperfine --warmup 3 --runs 20 --export-json "$dir/cold.json" \
    "$bin --exit-after-first-frame" >"$dir/hyperfine.out" 2>&1; then
    # hyperfine 2.x: .results[0].summary.time_wall_clock.mean; 1.x: .results[0].mean
    mean=$(jq -r '(.results[0].summary.time_wall_clock.mean // .results[0].mean) * 10000 | round / 10' "$dir/cold.json")
    echo "COLD_START_MS_MEAN=$mean" >>"$out"
else
    echo "COLD_START_MS_MEAN=failed" >>"$out"
    tail -n 20 "$dir/hyperfine.out" >>"$out"
fi

# Peak RSS cross-check on --smoke (the in-process rss_mb above is idle RSS).
case "$(uname -s)" in
    Darwin)
        peak=$(/usr/bin/time -l "$bin" --smoke 2>&1 >/dev/null |
            awk '/maximum resident set size/ { printf "%.1f", $1 / 1048576 }')
        ;;
    Linux)
        peak=$(/usr/bin/time -v "$bin" --smoke 2>&1 >/dev/null |
            awk '/Maximum resident set size/ { printf "%.1f", $NF / 1024 }')
        ;;
    *) peak= ;;
esac
echo "PEAK_RSS_MB=${peak:-n/a}" >>"$out"
echo >>"$out"

cat "$out"
exit "$smoke_status"
