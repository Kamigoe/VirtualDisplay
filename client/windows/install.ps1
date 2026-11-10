# Creates Start menu shortcuts (Start > VirtualDisplay) for the scripts in this folder.
# The shortcuts run the scripts where they are, so re-run this only if you move the repository.
# Run: powershell -ExecutionPolicy Bypass -File client\windows\install.ps1
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'VirtualDisplay'
New-Item -ItemType Directory -Force $menu | Out-Null
$sys = Join-Path $env:SystemRoot 'System32'
$ps = Join-Path $sys 'WindowsPowerShell\v1.0\powershell.exe'
$shell = New-Object -ComObject WScript.Shell

function New-Shortcut($name, $script, $icon, [switch]$console) {
    $lnk = $shell.CreateShortcut((Join-Path $menu "$name.lnk"))
    $psArgs = "-NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $here $script)`""
    if ($console) {
        $lnk.TargetPath = $ps
        $lnk.Arguments = $psArgs
    } else {
        # conhost --headless runs the console host without any window (powershell's own
        # -WindowStyle Hidden still flashes one, or a whole Windows Terminal tab).
        $lnk.TargetPath = Join-Path $sys 'conhost.exe'
        $lnk.Arguments = "--headless `"$ps`" $psArgs"
    }
    $lnk.WorkingDirectory = $here
    $lnk.IconLocation = $icon
    $lnk.Save()
}

$imageres = Join-Path $sys 'imageres.dll'
New-Shortcut 'Mac (VirtualDisplay)' 'vdview-mac.ps1' "$imageres,104"               # monitor
New-Shortcut 'Mac Wi-Fi Restart (VirtualDisplay)' 'mac-wifi-restart.ps1' "$imageres,20" # network
New-Shortcut 'Mac Unlock after Restart (VirtualDisplay)' 'mac-unlock.ps1' "$imageres,54" -console # padlock
New-Shortcut 'Mac Screen Sharing (VNC)' 'mac-vnc.ps1' "$imageres,139"                  # remote screen
Write-Host "installed shortcuts in $menu"
