# Opens the Mac's built-in Screen Sharing (VNC) in TigerVNC Viewer, e.g. to log in when the Mac
# stops at the login window after a restart (the virtual display only exists once vdsend runs).
# TigerVNC only does plain VNC password authentication, so on the Mac turn on
# Screen Sharing > (i) > "VNC viewers may control screen with password".
# Settings: VD_MAC_IP, VD_VNCVIEWER (path to vncviewer.exe).
$macIp = if ($env:VD_MAC_IP) { $env:VD_MAC_IP } else { '192.168.11.18' }
$viewer = $env:VD_VNCVIEWER
if (-not $viewer) {
    $cands = @(
        (Join-Path $env:ProgramFiles 'TigerVNC\vncviewer.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'TigerVNC\vncviewer.exe')
    )
    $viewer = $cands | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $viewer) {
    $cmd = Get-Command vncviewer.exe, vncviewer64*.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) { $viewer = $cmd.Source }
}
if (-not $viewer) {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show('TigerVNC Viewer (vncviewer.exe) was not found. Install it or set VD_VNCVIEWER.', 'Mac Screen Sharing') | Out-Null
    exit 1
}
Start-Process -FilePath $viewer -ArgumentList "${macIp}::5900"
