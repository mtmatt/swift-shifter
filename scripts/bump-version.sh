#!/usr/bin/env bash
# Usage: scripts/bump-version.sh <version>
# Updates the version in package.json, Cargo.toml, and tauri.conf.json,
# then commits the change. Pushing to main will auto-create the git tag.
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
# Replace only the [package] version line (first occurrence) to avoid
# accidentally touching dependency version strings.
if [[ "$(uname)" == "Darwin" ]]; then
  sed -i '' "0,/^version = \"[^\"]*\"/s//version = \"$VERSION\"/" swift-shifter/Cargo.toml
else
  sed -i "0,/^version = \"[^\"]*\"/s//version = \"$VERSION\"/" swift-shifter/Cargo.toml
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
echo ""
echo "Pushing to main will trigger the auto-tag workflow which creates v$VERSION,"
echo "which in turn triggers the release workflow."
