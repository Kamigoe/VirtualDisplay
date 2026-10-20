#!/bin/sh
# Builds vdsend, installs it as a signed app bundle under ~/Library/VirtualDisplay, and registers
# two LaunchAgents:
#   com.kamigoe.vdsend         start vdsend at login, restart it if it exits
#   com.kamigoe.vdsend.wifi    Wi-Fi watchdog (every 20 s)
#
# Usage: scripts/install.sh [vdsend options...]   (default: --mode 3440x1440@60 --main)
# Logs:  ~/Library/Logs/vdsend.log, ~/Library/Logs/vdsend-wifi.log
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
DEST="$HOME/Library/VirtualDisplay"
APP="$DEST/vdsend.app"
AGENTS="$HOME/Library/LaunchAgents"
LABEL=com.kamigoe.vdsend
IDENTITY="VirtualDisplay Code Signing"
UIDN=$(id -u)
[ $# -gt 0 ] || set -- --mode 3440x1440@60 --main
WAS_RUNNING=$(launchctl print "gui/$UIDN/$LABEL" >/dev/null 2>&1 && echo 1 || true)

echo "==> building"
(cd "$ROOT" && swift build -c release 2>&1 | tail -1)

echo "==> installing $APP"
mkdir -p "$APP/Contents/MacOS" "$AGENTS" "$HOME/Library/Logs"
cp "$ROOT/.build/release/vdsend" "$APP/Contents/MacOS/vdsend"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$LABEL</string>
  <key>CFBundleName</key><string>vdsend</string>
  <key>CFBundleExecutable</key><string>vdsend</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
cp "$HERE/wifi-restart.sh" "$HERE/wifi-watchdog.sh" "$DEST/"
chmod +x "$DEST/wifi-restart.sh" "$DEST/wifi-watchdog.sh"

echo "==> signing (first time: allow codesign to use the key in the keychain dialog)"
"$HERE/make-signing-identity.sh"
codesign --force --sign "$IDENTITY" --identifier "$LABEL" "$APP"

echo "==> LaunchAgents"
ARGS=""
for a in "$APP/Contents/MacOS/vdsend" "$@"; do ARGS="$ARGS    <string>$a</string>
"; done
cat >"$AGENTS/$LABEL.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
$ARGS  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/vdsend.log</string>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/vdsend.log</string>
</dict></plist>
PLIST
cat >"$AGENTS/$LABEL.wifi.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL.wifi</string>
  <key>ProgramArguments</key><array><string>$DEST/wifi-watchdog.sh</string></array>
  <key>StartInterval</key><integer>20</integer>
  <key>RunAtLoad</key><true/>
</dict></plist>
PLIST

for l in "$LABEL" "$LABEL.wifi"; do
    launchctl bootout "gui/$UIDN/$l" 2>/dev/null || true
    # bootout returns before the job is gone; bootstrapping too early fails with EIO.
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        launchctl print "gui/$UIDN/$l" >/dev/null 2>&1 || break
        sleep 0.5
    done
done
launchctl bootstrap "gui/$UIDN" "$AGENTS/$LABEL.wifi.plist"
if [ -n "$WAS_RUNNING" ]; then
    launchctl bootstrap "gui/$UIDN" "$AGENTS/$LABEL.plist"
    echo "restarted $LABEL (update)"
    exit 0
fi

echo
echo "Screen Recording permission for vdsend.app:"
echo "  if not granted yet, run:  open -a \"$APP\" --args --request-permission"
echo "  then enable 'vdsend' in System Settings > Privacy & Security > Screen & System Audio Recording."
echo
echo "Start the agent (stop any vdsend started by hand first):"
echo "  launchctl bootstrap gui/$UIDN \"$AGENTS/$LABEL.plist\""
