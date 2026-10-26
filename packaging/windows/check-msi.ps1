# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks the MSI built by cargo-dist (ADR-0075): silent install, the exe, the Start menu shortcut, the machine PATH,
# `--version`, then uninstall. INSTALLS ON THE HOST: run it on CI runners only.
#
#   pwsh -NoProfile -File packaging/windows/check-msi.ps1 -Msi <path>
param([Parameter(Mandatory = $true)][string]$Msi)
$ErrorActionPreference = 'Stop'

$log = Join-Path ([IO.Path]::GetTempPath()) 'msi-install.log'
$uninstallLog = Join-Path ([IO.Path]::GetTempPath()) 'msi-uninstall.log'

function Fail([string]$reason, [string]$logFile = $log) {
    Write-Host "::error title=Installer::$reason"
    if (Test-Path $logFile) { Get-Content $logFile -Tail 60 | ForEach-Object { Write-Host $_ } }
    exit 1
}

if (-not (Test-Path $Msi)) { Write-Host "usage: check-msi.ps1 -Msi <path>"; exit 2 }
$msiPath = (Resolve-Path $Msi).Path

$p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msiPath`"", '/qn', '/l*v', "`"$log`"") -Wait -PassThru
if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { Fail "msiexec /i exited with $($p.ExitCode)" }

$exe = Join-Path $env:ProgramFiles 'sqler-app\bin\sqler.exe'
$lnk = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\SQLer.lnk'
if (-not (Test-Path $exe)) { Fail "$exe does not exist after install" }
if (-not (Test-Path $lnk)) { Fail "$lnk does not exist after install" }

$envKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
$machinePath = (Get-ItemProperty -Path $envKey -Name Path).Path
if ($machinePath -notlike '*\sqler-app\bin*') { Fail 'the machine PATH does not contain \sqler-app\bin' }

# The GUI-subsystem exe has no console: redirect its output to a file.
$out = Join-Path ([IO.Path]::GetTempPath()) 'sqler-version.txt'
Start-Process $exe -ArgumentList '--version' -Wait -RedirectStandardOutput $out -NoNewWindow | Out-Null
$ver = (Get-Content $out -Raw)
Write-Host $ver
if ($ver -notmatch '^sqler [0-9]') { Fail '--version does not match ^sqler [0-9]' }

$p = Start-Process msiexec.exe -ArgumentList @('/x', "`"$msiPath`"", '/qn', '/l*v', "`"$uninstallLog`"") -Wait -PassThru
if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { Fail "msiexec /x exited with $($p.ExitCode)" $uninstallLog }
if (Test-Path $exe) { Fail "$exe is still there after uninstall" $uninstallLog }
if (Test-Path $lnk) { Fail "$lnk is still there after uninstall" $uninstallLog }
Write-Host 'check-msi: ok'
