#!/bin/sh
# Installs the launcher scripts and desktop entries for the current user.
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
install -m 755 "$HERE/vdview-mac" "$HERE/mac-wifi-restart" "$HOME/.local/bin/"
install -m 644 "$HERE/vdview-mac.desktop" "$HERE/mac-wifi-restart.desktop" "$HOME/.local/share/applications/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "installed: vdview-mac, mac-wifi-restart (+ app menu entries)"
