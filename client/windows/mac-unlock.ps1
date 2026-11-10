# Unlocks the Mac's FileVault after a restart, using macOS 26's pre-boot SSH (password only).
# Waits for the Mac to appear on the network (looking it up by hardware address if its IP
# changed), then connects; type the Mac login password when asked.
# Settings: VD_MAC_IP, VD_MAC_HW, VD_MAC_USER.
$macIp = if ($env:VD_MAC_IP) { $env:VD_MAC_IP } else { '192.168.11.18' }
$macHw = if ($env:VD_MAC_HW) { $env:VD_MAC_HW } else { '50:ed:3c:1f:f5:93' }
$user = if ($env:VD_MAC_USER) { $env:VD_MAC_USER } else { 'akira' }
$prefix = $macIp.Substring(0, $macIp.LastIndexOf('.'))

function Test-Port([string]$ip, [int]$port) {
    $c = New-Object System.Net.Sockets.TcpClient
    try {
        $t = $c.ConnectAsync($ip, $port)
        return $t.Wait(2000) -and $c.Connected
    } catch {
        return $false
    } finally {
        $c.Dispose()
    }
}

function Find-ByHw {
    # Ping the whole /24 at once so the neighbor (ARP) table is filled, then look the Mac up.
    $pings = 1..254 | ForEach-Object {
        (New-Object System.Net.NetworkInformation.Ping).SendPingAsync("$prefix.$_", 1000)
    }
    try { [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]$pings) } catch {}
    $hw = $macHw.Replace(':', '-').ToUpper()
    Get-NetNeighbor -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object { $_.LinkLayerAddress.ToUpper() -eq $hw -and $_.IPAddress.StartsWith("$prefix.") } |
        Select-Object -First 1 -ExpandProperty IPAddress
}

function Wait-Close {
    Read-Host 'Press Enter to close' | Out-Null
}

Write-Host "Waiting for the Mac ($macIp / $macHw) to come up... (Ctrl-C to stop)"
for ($i = 1; $i -le 120; $i++) {
    if (Test-Port $macIp 22) { break }
    if ($i % 10 -eq 0) {
        $found = Find-ByHw
        if ($found -and $found -ne $macIp) {
            Write-Host "Mac found at $found"
            $macIp = $found
        }
    }
    Start-Sleep -Seconds 2
}
if (-not (Test-Port $macIp 22)) {
    Write-Host "The Mac did not show up. Fall back to typing the password on the Mac's own keyboard."
    Wait-Close
    exit 1
}

Write-Host "Connecting to $user@$macIp - enter the Mac login password to unlock FileVault."
# The pre-boot environment has its own host key; keep it apart from the normal one.
$knownHosts = Join-Path $HOME '.ssh\known_hosts_mac_preboot'
& ssh.exe -o "UserKnownHostsFile=$knownHosts" -o StrictHostKeyChecking=accept-new `
    -o PubkeyAuthentication=no -o PreferredAuthentications=keyboard-interactive,password `
    "$user@$macIp"
Write-Host ''
# Unlocking over SSH does not log in to the desktop (typing the password on the Mac does), so the
# Mac stops at the login window. Open Screen Sharing for that as soon as it is available.
Write-Host "Unlocked. Waiting for the login window's Screen Sharing (VNC)..."
for ($i = 1; $i -le 90; $i++) {
    if (Test-Port $macIp 5900) {
        Write-Host 'Log in to the Mac in the VNC viewer window, then close it.'
        Write-Host "'Mac (VirtualDisplay)' reconnects by itself once you are logged in."
        $env:VD_MAC_IP = $macIp
        & (Join-Path $PSScriptRoot 'mac-vnc.ps1')
        Start-Sleep -Seconds 3
        exit 0
    }
    Start-Sleep -Seconds 2
}
Write-Host "Screen Sharing did not come up; type the password on the Mac's keyboard to log in."
Wait-Close
