# Turns the Mac's Wi-Fi off and on (for a flaky link). Uses ~/.ssh/vd_mac, which the Mac only
# accepts for this one command. Falls back to the last known IP if mDNS does not resolve.
# Settings: VD_MAC, VD_MAC_IP, VD_MAC_USER.
$mac = if ($env:VD_MAC) { $env:VD_MAC } else { 'ikedaakiranoMacBook-Air.local' }
$macIp = if ($env:VD_MAC_IP) { $env:VD_MAC_IP } else { '192.168.11.18' }
$user = if ($env:VD_MAC_USER) { $env:VD_MAC_USER } else { 'akira' }
$key = Join-Path $HOME '.ssh\vd_mac'

function Show-Note([string]$text) {
    # A tray balloon shows up as a Windows notification; no extra modules needed.
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $n = New-Object System.Windows.Forms.NotifyIcon
    $n.Icon = [System.Drawing.SystemIcons]::Information
    $n.Visible = $true
    $n.ShowBalloonTip(5000, 'Mac Wi-Fi', $text, [System.Windows.Forms.ToolTipIcon]::None)
    Start-Sleep -Seconds 6
    $n.Dispose()
}

foreach ($h in @($mac, $macIp)) {
    & ssh.exe -i $key -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new "$user@$h" restart 2>$null
    if ($LASTEXITCODE -eq 0) {
        Show-Note 'Restarting Wi-Fi on the Mac (back in ~10 s)'
        exit 0
    }
}
Write-Error 'could not reach the Mac'
Show-Note 'Could not reach the Mac'
exit 1
