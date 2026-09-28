#!/usr/bin/env bash
# Usage: scripts/bump-version.sh <version>
# Updates the version in package.json, Cargo.toml, and tauri.conf.json.
# Committing, tagging, and pushing are left to you (see the steps printed at
# the end); scripts/check-version.sh verifies the result.
set -euo pipefail

VERSION="${1:?Usage: scripts/bump-version.sh <version>}"
VERSION="${VERSION#v}"  # strip leading 'v' if present

# Validate semver format
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
  echo "Error: '$VERSION' is not a valid semver (expected e.g. 1.2.3 or 1.2.3-beta.1)" >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# --- package.json ---
npm version "$VERSION" --no-git-tag-version --allow-same-version

# --- swift-shifter/Cargo.toml ---
# Replace the [package] version line. Dependency versions are all written as
# `crate = { version = "..." }` (not at the start of a line), so ^version =
# only matches the package declaration.
if [[ "$(uname)" == "Darwin" ]]; then
  sed -i '' "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" swift-shifter/Cargo.toml
else
  sed -i "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" swift-shifter/Cargo.toml
fi

# --- swift-shifter/tauri.conf.json ---
node - <<EOF
const fs = require('fs');
const path = 'swift-shifter/tauri.conf.json';
const cfg = JSON.parse(fs.readFileSync(path, 'utf8'));
cfg.version = '$VERSION';
fs.writeFileSync(path, JSON.stringify(cfg, null, 2) + '\n');
EOF

echo "Bumped all version files to $VERSION"
echo ""
echo "Review the diff, then:"
echo "  git add package.json swift-shifter/Cargo.toml swift-shifter/tauri.conf.json"
echo "  git commit -m \"chore: bump version to $VERSION\""
echo "  git push"
echo "  git tag v$VERSION && git push origin v$VERSION"
echo ""
echo "Pushing the tag triggers the release workflow, which first checks that"
echo "the tag matches all three version files."
