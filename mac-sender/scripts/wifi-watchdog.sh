#!/bin/sh
# Run every 20 s by launchd. Restarts Wi-Fi when the default gateway has been unreachable for three
# checks in a row (~1 min), and turns Wi-Fi back on if it is off: on this Mac the network is the
# only way to reach the screen.
DIR="$(cd "$(dirname "$0")" && pwd)"
STATE="${TMPDIR:-/tmp}/vdsend-wifi-watchdog.fails"
LOG="$HOME/Library/Logs/vdsend-wifi.log"
IF=$(/usr/sbin/networksetup -listallhardwareports | awk '/Hardware Port: Wi-Fi/{getline; print $2; exit}')
[ -n "$IF" ] || IF=en0

if [ "$(/usr/sbin/networksetup -getairportpower "$IF" | awk '{print $NF}')" = "Off" ]; then
    echo "$(date '+%F %T') Wi-Fi was off; turning it on" >>"$LOG"
    /usr/sbin/networksetup -setairportpower "$IF" on
    exit 0
fi

GW=$(/sbin/route -n get default 2>/dev/null | awk '/gateway:/{print $2}')
if [ -n "$GW" ] && /sbin/ping -c 2 -t 3 -q "$GW" >/dev/null 2>&1; then
    rm -f "$STATE"
    exit 0
fi

FAILS=$(( $(cat "$STATE" 2>/dev/null || echo 0) + 1 ))
echo "$FAILS" >"$STATE"
echo "$(date '+%F %T') gateway ${GW:-<none>} unreachable ($FAILS)" >>"$LOG"
if [ "$FAILS" -ge 3 ]; then
    rm -f "$STATE"
    "$DIR/wifi-restart.sh" --detached watchdog
fi
