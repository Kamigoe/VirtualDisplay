#!/bin/sh
# Turns Wi-Fi off and on again. Detaches first, so it also completes when triggered over the very
# SSH connection (or stream) it is about to cut.
# Log: ~/Library/Logs/vdsend-wifi.log
LOG="$HOME/Library/Logs/vdsend-wifi.log"
if [ "$1" != "--detached" ]; then
    nohup "$0" --detached "$@" >/dev/null 2>&1 &
    echo "Wi-Fi restart scheduled"
    exit 0
fi
IF=$(/usr/sbin/networksetup -listallhardwareports | awk '/Hardware Port: Wi-Fi/{getline; print $2; exit}')
[ -n "$IF" ] || IF=en0
sleep 1
echo "$(date '+%F %T') restarting Wi-Fi on $IF ($2)" >>"$LOG"
/usr/sbin/networksetup -setairportpower "$IF" off
sleep 3
/usr/sbin/networksetup -setairportpower "$IF" on
echo "$(date '+%F %T') Wi-Fi power: $(/usr/sbin/networksetup -getairportpower "$IF" | awk '{print $NF}')" >>"$LOG"
