#!/bin/sh
# Creates a self-signed code-signing identity in the login keychain. Signing vdsend with a stable
# identity keeps its Screen Recording permission across rebuilds (ad-hoc signatures change with
# every build, and macOS then forgets the grant).
set -e
NAME="VirtualDisplay Code Signing"
if security find-certificate -c "$NAME" >/dev/null 2>&1; then
    echo "identity \"$NAME\" already exists"
    exit 0
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cfg" <<CFG
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CFG
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$TMP/cfg" \
    -keyout "$TMP/key.pem" -out "$TMP/cert.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" \
    -out "$TMP/id.p12" -passout pass:vd
security import "$TMP/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" -P vd -T /usr/bin/codesign
echo "created identity \"$NAME\""
