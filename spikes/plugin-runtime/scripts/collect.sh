#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Merge every results.txt below <artifacts-dir> into <out-dir>/summary.md and summary.txt.
# Usage: collect.sh <artifacts-dir> <out-dir>
#   SQLER_LINUX_BYTES (optional): newest size.yml Linux figure of the shipped binary.
#   Headroom = 62914560 - SQLER_LINUX_BYTES; without it the headroom columns read n/a.
#   CRLF is stripped on read. The OS of a file is its `SPIKE os=` key (linux when absent).
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <artifacts-dir> <out-dir>" >&2; exit 2; }
[ -d "$1" ] || { echo "no such directory: $1" >&2; exit 2; }
mkdir -p "$2"
out=$(cd "$2" && pwd)
src=$(cd "$1" && pwd)
budget=62914560
linux_bytes=${SQLER_LINUX_BYTES:-}

norm="$out/.norm.tmp"
: >"$norm"
find "$src" -name results.txt | LC_ALL=C sort | while IFS= read -r f; do
    tr -d '\r' <"$f" | awk '
        BEGIN { n = 0 }
        /^SPIKE [A-Za-z0-9_]+=/ {
            line = substr($0, 7); i = index(line, "=")
            k[n] = substr(line, 1, i - 1); v[n] = substr(line, i + 1); n++
            if (k[n - 1] == "os") os = v[n - 1]
            if (k[n - 1] == "variant") var = v[n - 1]
        }
        END {
            if (os == "") os = "linux"
            if (var == "") var = "-"
            for (j = 0; j < n; j++) printf "%s\t%s\t%s\t%s\n", os, var, k[j], v[j]
        }' >>"$norm"
done

# summary.txt: all keys prefixed by OS (and the variant when the file has one).
LC_ALL=C sort -u "$norm" | awk -F'\t' '{
    p = ($2 == "-") ? $1 "." : $1 "." $2 "."
    printf "SPIKE %s%s=%s\n", p, $3, $4
}' >"$out/summary.txt"

{
    echo "# Plugin runtime spike summary"
    echo
    echo "## Binary size (release profile, delta = variant - base on the same OS)"
    echo
    if [ -n "$linux_bytes" ]; then
        echo "Linux shipped binary (size.yml): $linux_bytes bytes; headroom under $budget: $((budget - linux_bytes)) bytes."
    else
        echo "Linux shipped binary (size.yml): n/a; headroom columns not computed."
    fi
    echo
    echo "| OS | variant | bytes | delta bytes | delta KiB | Linux headroom after delta (bytes) |"
    echo "| -- | ------- | ----- | ----------- | --------- | ---------------------------------- |"
    awk -F'\t' -v lb="$linux_bytes" -v budget="$budget" '
        $3 == "binary_bytes" { b[$1 SUBSEP $2] = $4 }
        $3 == "winch" { na[$1 SUBSEP $2] = $4 }
        END {
            no = split("linux macos windows", os, " ")
            nv = split("base cl cl-default pulley pulley-rt winch", vs, " ")
            for (i = 1; i <= no; i++) for (j = 1; j <= nv; j++) {
                key = os[i] SUBSEP vs[j]
                if (!(key in b)) continue
                bytes = b[key]
                if ((os[i] SUBSEP "base") in b) {
                    d = bytes - b[os[i] SUBSEP "base"]
                    ds = sprintf("%d", d); dk = sprintf("%.1f", d / 1024)
                } else { ds = "n/a"; dk = "n/a" }
                h = "n/a"
                if (os[i] == "linux" && lb != "" && ds != "n/a") h = sprintf("%d", budget - lb - d)
                printf "| %s | %s | %d | %s | %s | %s |\n", os[i], vs[j], bytes, ds, dk, h
            }
        }' "$norm"
    echo
    echo "## Latency, memory, fuel and epoch (hello fixture)"
    echo
    echo "| OS | variant | compile ms | load cwasm ms | instantiate p50/p95 us | call p50/p95 ns | idle KiB/instance | spin none/fuel/epoch ms | fuel/epoch overhead % | interrupt ok fuel/epoch |"
    echo "| -- | ------- | ---------- | ------------- | --------------------- | --------------- | ----------------- | ----------------------- | --------------------- | ----------------------- |"
    awk -F'\t' '
        { m[$1 SUBSEP $2 SUBSEP $3] = $4 }
        $3 == "call_p50_ns" { has[$1 SUBSEP $2] = 1 }
        function g(o, v, k) { return ((o SUBSEP v SUBSEP k) in m) ? m[o SUBSEP v SUBSEP k] : "n/a" }
        END {
            no = split("linux macos windows", os, " ")
            nv = split("cl cl-default pulley pulley-rt winch", vs, " ")
            for (i = 1; i <= no; i++) for (j = 1; j <= nv; j++) {
                o = os[i]; v = vs[j]
                if (!((o SUBSEP v) in has)) continue
                printf "| %s | %s | %s | %s | %s / %s | %s / %s | %s | %s / %s / %s | %s / %s | %s / %s |\n", o, v,
                    g(o, v, "compile_ms"), g(o, v, "load_cwasm_ms"),
                    g(o, v, "instantiate_p50_us"), g(o, v, "instantiate_p95_us"),
                    g(o, v, "call_p50_ns"), g(o, v, "call_p95_ns"), g(o, v, "idle_instance_kib"),
                    g(o, v, "spin_none_ms"), g(o, v, "spin_fuel_ms"), g(o, v, "spin_epoch_ms"),
                    g(o, v, "fuel_overhead_pct"), g(o, v, "epoch_overhead_pct"),
                    g(o, v, "fuel_interrupt_ok"), g(o, v, "epoch_interrupt_ok")
            }
        }' "$norm"
    echo
    echo "## Guests and toolchain"
    echo
    echo "| OS | key | value |"
    echo "| -- | --- | ----- |"
    awk -F'\t' '$3 ~ /^(toolchain_|hello_|guest_)/ { printf "%s\t%s\t%s\n", $1, $3, $4 }' "$norm" |
        LC_ALL=C sort -u | awk -F'\t' '{ printf "| %s | %s | %s |\n", $1, $2, $3 }'
} >"$out/summary.md"

rm -f "$norm"
echo "wrote $out/summary.md and $out/summary.txt"
