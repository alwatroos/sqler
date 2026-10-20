# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Spike measurement procedure for Windows (measure.sh is the Linux and macOS twin).
# Usage: measure.ps1 <path-to-spike-binary>
# Appends to $env:RESULTS_DIR\results.txt (default: current directory) and writes cold.json.
param([Parameter(Mandatory = $true)][string]$Bin)

$Bin = (Resolve-Path $Bin).Path

$dir = if ($env:RESULTS_DIR) { $env:RESULTS_DIR } else { '.' }
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$out = Join-Path $dir 'results.txt'

Add-Content $out "== date: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))"
Add-Content $out "== host: $([System.Environment]::OSVersion.VersionString)"
Add-Content $out '== smoke'

$smokeOut = Join-Path $dir 'smoke.out'
$smokeErr = Join-Path $dir 'smoke.err'
$p = Start-Process -FilePath $Bin -ArgumentList '--smoke' -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput $smokeOut -RedirectStandardError $smokeErr
$smokeStatus = $p.ExitCode
Get-Content $smokeOut | Add-Content $out
Add-Content $out "SMOKE_EXIT=$smokeStatus"
if ($smokeStatus -ne 0) {
    Add-Content $out '== smoke stderr (tail)'
    Get-Content $smokeErr -Tail 40 | Add-Content $out
}

$bytes = (Get-Item $Bin).Length
Add-Content $out "BINARY_BYTES=$bytes"
Add-Content $out ("BINARY_MB={0:N1}" -f ($bytes / 1MB))

$cold = Join-Path $dir 'cold.json'
hyperfine -N --warmup 3 --runs 20 --export-json $cold "`"$Bin`" --exit-after-first-frame" *>&1 |
    Tee-Object -FilePath (Join-Path $dir 'hyperfine.out') | Out-Host
if ($LASTEXITCODE -eq 0) {
    # hyperfine 2.x: results[0].summary.time_wall_clock.mean; 1.x: results[0].mean
    $r = (Get-Content $cold -Raw | ConvertFrom-Json).results[0]
    $m = if ($r.summary) { $r.summary.time_wall_clock.mean } else { $r.mean }
    $mean = $m * 1000
    Add-Content $out ("COLD_START_MS_MEAN={0:N1}" -f $mean)
} else {
    Add-Content $out 'COLD_START_MS_MEAN=failed'
}
Add-Content $out 'PEAK_RSS_MB=n/a (in-process rss_mb above)'
Add-Content $out ''

Get-Content $out
exit $smokeStatus
