# Show the Mac's virtual display (vdsend must be running on the Mac); vdview reconnects by itself.
# Extra arguments are passed to vdview, e.g. --chroma 420. Log: %LOCALAPPDATA%\vdview\vdview.log
# Settings: VD_MAC (Mac host name), VD_VDVIEW (path to vdview.exe).
$ErrorActionPreference = 'Stop'
$mac = if ($env:VD_MAC) { $env:VD_MAC } else { 'ikedaakiranoMacBook-Air.local' }
$exe = if ($env:VD_VDVIEW) { $env:VD_VDVIEW } else { Join-Path $PSScriptRoot '..\target\release\vdview.exe' }
$logDir = Join-Path $env:LOCALAPPDATA 'vdview'
$log = Join-Path $logDir 'vdview.log'
New-Item -ItemType Directory -Force $logDir | Out-Null
Add-Content -Path $log -Encoding ASCII -Value ("=== {0} vdview-mac {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), ($args -join ' '))
if (-not (Test-Path $exe)) {
    Add-Content -Path $log -Encoding ASCII -Value "vdview.exe not found: $exe (build it with cargo build --release)"
    exit 1
}
# cmd appends vdview's raw output (PowerShell's own redirection would re-encode it and wrap stderr).
$quoted = @($exe, $mac) + $args | ForEach-Object { '"' + $_ + '"' }
& cmd.exe /d /c ('"' + ($quoted -join ' ') + ' >>"' + $log + '" 2>&1"')
exit $LASTEXITCODE
