#!/usr/bin/env bash
# Usage: scripts/check-version.sh [tag]
# Fails unless package.json, swift-shifter/Cargo.toml, and
# swift-shifter/tauri.conf.json declare the same version. When a tag is given
# (e.g. v1.2.3), it must match that version too. Run by CI on every build and
# before the release build, since tags are pushed by hand.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

json_version() {
  node -p "require('./$1').version"
}

npm_version="$(json_version package.json)"
tauri_version="$(json_version swift-shifter/tauri.conf.json)"
# The [package] version is the only line starting with `version =`
# (see bump-version.sh).
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' swift-shifter/Cargo.toml)"

echo "package.json:     $npm_version"
echo "Cargo.toml:       $cargo_version"
echo "tauri.conf.json:  $tauri_version"

if [[ "$npm_version" != "$cargo_version" || "$npm_version" != "$tauri_version" ]]; then
  echo "Error: version files disagree; run scripts/bump-version.sh <version>" >&2
  exit 1
fi

if [[ $# -ge 1 ]]; then
  echo "tag:              $1"
  if [[ "$1" != v* ]]; then
    echo "Error: tag $1 must look like v<version>" >&2
    exit 1
  fi
  tag_version="${1#v}"
  if [[ "$tag_version" != "$npm_version" ]]; then
    echo "Error: tag $1 does not match version $npm_version" >&2
    exit 1
  fi
fi

echo "OK: $npm_version"
