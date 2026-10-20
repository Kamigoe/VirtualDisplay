#!/bin/sh
# Removes the LaunchAgents and the installed app (keeps logs and the signing identity).
UIDN=$(id -u)
for l in com.kamigoe.vdsend com.kamigoe.vdsend.wifi; do
    launchctl bootout "gui/$UIDN/$l" 2>/dev/null && echo "stopped $l"
    rm -f "$HOME/Library/LaunchAgents/$l.plist"
done
rm -rf "$HOME/Library/VirtualDisplay"
echo "removed ~/Library/VirtualDisplay"
