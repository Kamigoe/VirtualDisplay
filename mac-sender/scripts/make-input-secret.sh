#!/bin/sh
# Creates the input secret that lets vdview send keyboard and mouse input to this Mac
# (~/Library/VirtualDisplay/input-secret, readable only by you) and prints it, so that it can be
# saved on the PC with `vdview --set-input-secret`. Keeps an existing secret unless --new is given.
#
# Usage: scripts/make-input-secret.sh [--new]
set -e
DEST="$HOME/Library/VirtualDisplay"
FILE="$DEST/input-secret"
umask 077
mkdir -p "$DEST"
if [ -s "$FILE" ] && [ "$1" != "--new" ]; then
    echo "keeping the existing secret (scripts/make-input-secret.sh --new replaces it;"
    echo "every PC then needs the new one)"
else
    # 32 random bytes as 64 hex digits.
    od -An -N32 -tx1 /dev/urandom | tr -d ' \n' >"$FILE"
    echo >>"$FILE"
    echo "created $FILE"
fi
chmod 600 "$FILE"
echo
echo "input secret (keep it private; enter it on each PC with: vdview --set-input-secret):"
cat "$FILE"
