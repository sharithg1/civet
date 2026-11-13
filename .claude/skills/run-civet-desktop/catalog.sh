#!/usr/bin/env bash
# Capture the full UI catalog (tour.txt) into the repo for a design review:
# <dir>/screenshots/*.png + manifest.json, and <dir>/README.md (index + review brief).
#
#   catalog.sh [dir (default docs/ui-review)] [size (default 1400x900)]
set -euo pipefail
export PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH"

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
dir="${1:-docs/ui-review}"
size="${2:-1400x900}"
cd "$repo"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

"$here/shoot.sh" "$tmp" --sizes "$size"
rm -rf "$dir/screenshots"
mkdir -p "$dir"
mv "$tmp/after/$size" "$dir/screenshots"
node "$here/index.mjs" "$dir"
du -sh "$dir/screenshots"
