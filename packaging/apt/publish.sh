#!/usr/bin/env bash
# Build the .deb and assemble a signed reprepro APT repo under apt-repo/.
#
# Requires: cargo-deb, reprepro, gpg with a usable signing key. Set
# REPREPRO_SIGN_KEY to the key id. This produces files to publish; it does NOT
# upload them anywhere.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

: "${REPREPRO_SIGN_KEY:?set REPREPRO_SIGN_KEY to your GPG key id}"

"$ROOT/packaging/deb/build.sh"

REPO="$ROOT/apt-repo"
mkdir -p "$REPO/conf"
cat > "$REPO/conf/distributions" <<EOF
Origin: ripsync
Label: ripsync
Codename: stable
Architectures: amd64 arm64
Components: main
Description: ripsync APT repository
SignWith: ${REPREPRO_SIGN_KEY}
EOF

reprepro -b "$REPO" includedeb stable "$ROOT"/target/debian/ripsync_*.deb
gpg --armor --export "$REPREPRO_SIGN_KEY" > "$REPO/ripsync-archive-keyring.asc"

echo ">> apt repo ready under $REPO (publish this tree to Pages/your host)"
