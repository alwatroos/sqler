# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Checks the MSI built by cargo-dist (ADR-0075): the icon tables, silent install, the exe, the Start menu
# shortcut and the icons, the machine PATH, `--version`, then uninstall. INSTALLS ON THE HOST: run it on CI runners only.
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

# The icon is in the MSI itself: an Icon table row and the ARP product icon (F6).
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($msiPath, 0))
function Get-MsiValues([string]$query) {
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($query))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $values = @()
    while ($true) {
        $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $rec) { break }
        $values += $rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, @(1))
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    return $values
}
$iconNames = @(Get-MsiValues 'SELECT Name FROM Icon')
if ($iconNames -notcontains 'SqlerIcon.ico') { Fail "the MSI Icon table has no SqlerIcon.ico (found: $($iconNames -join ', '))" }
$arpIcon = @(Get-MsiValues "SELECT Value FROM Property WHERE Property = 'ARPPRODUCTICON'")
if ($arpIcon -notcontains 'SqlerIcon.ico') { Fail 'ARPPRODUCTICON is not SqlerIcon.ico' }

# Release the read-only handle before msiexec opens the same file.
$db = $null
$installer = $null
[GC]::Collect()
[GC]::WaitForPendingFinalizers()

$p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msiPath`"", '/qn', '/l*v', "`"$log`"") -Wait -PassThru
if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { Fail "msiexec /i exited with $($p.ExitCode)" }

$exe = Join-Path $env:ProgramFiles 'sqler-app\bin\sqler.exe'
$lnk = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\SQLer.lnk'
if (-not (Test-Path $exe)) { Fail "$exe does not exist after install" }
if (-not (Test-Path $lnk)) { Fail "$lnk does not exist after install" }

# The shortcut carries an icon, and the exe embeds one (AC5).
$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$iconLocation = $shortcut.IconLocation
if ([string]::IsNullOrEmpty($iconLocation) -or $iconLocation.StartsWith(',')) { Fail "the Start menu shortcut has no icon (IconLocation: '$iconLocation')" }
Add-Type -Namespace Sqler -Name Shell -MemberDefinition '[DllImport("shell32.dll", CharSet = CharSet.Unicode)] public static extern uint ExtractIconEx(string file, int index, IntPtr[] large, IntPtr[] small, uint icons);'
$iconCount = [Sqler.Shell]::ExtractIconEx($exe, -1, $null, $null, 0)
if ($iconCount -lt 1) { Fail "$exe embeds no icon (ExtractIconEx returned $iconCount)" }

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
