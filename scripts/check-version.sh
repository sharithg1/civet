#!/usr/bin/env bash
# Check that every file carrying the release version agrees, and optionally
# that it matches a tag.
#
#   scripts/check-version.sh            # Cargo.toml == tauri.conf.json; prints the version
#   scripts/check-version.sh v0.1.5     # ... and both == 0.1.5 (leading "v" optional)
#
# The desktop app refuses daemons built from another version, so the
# workspace version (Cargo.toml [workspace.package], used by every crate
# including civetd) and the app bundle version (tauri.conf.json) must match.
# Used by scripts/release.sh and the release workflow.
set -euo pipefail

cd "$(dirname "$0")/.."

# `version = "…"` inside [workspace.package] of the root Cargo.toml.
cargo_version() {
    awk '
        /^\[/ { in_pkg = ($0 == "[workspace.package]") }
        in_pkg && /^version[ \t]*=/ {
            v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); print v; exit
        }
    ' Cargo.toml
}

# Top-level "version" of tauri.conf.json (two-space indented key).
tauri_version() {
    sed -n 's/^  "version": *"\([^"]*\)".*/\1/p' crates/civet-desktop/tauri.conf.json | head -n1
}

cv=$(cargo_version)
tv=$(tauri_version)
status=0
[ -n "$cv" ] || { echo "error: no version in Cargo.toml [workspace.package]" >&2; status=1; }
[ -n "$tv" ] || { echo "error: no version in crates/civet-desktop/tauri.conf.json" >&2; status=1; }
if [ "$status" = 0 ] && [ "$cv" != "$tv" ]; then
    echo "error: Cargo.toml version $cv != tauri.conf.json version $tv" >&2
    status=1
fi
if [ $# -gt 0 ]; then
    want=${1#v}
    if [ "$cv" != "$want" ] || [ "$tv" != "$want" ]; then
        echo "error: tag $1 does not match Cargo.toml ($cv) / tauri.conf.json ($tv);" \
            "run scripts/release.sh $want and tag that commit" >&2
        status=1
    fi
fi
[ "$status" = 0 ] || exit 1
echo "$cv"
