#!/usr/bin/env bash
# Build static civetd binaries for Linux remote hosts with cargo-zigbuild.
#
#   scripts/build-remote.sh                      # both targets -> target/civet-dist/
#   scripts/build-remote.sh x86_64-unknown-linux-musl
#   scripts/build-remote.sh --out DIR            # put civetd-<triple> files in DIR
#   scripts/build-remote.sh --install            # also copy to ~/.civet/dist
#
# Output files are named civetd-<target-triple>, the layout the client
# bootstrap expects (BinarySource::Dir). Point clients at them with
#   export CIVET_DIST_DIR=<out dir>
# (civet-cli --dist DIR, or the desktop app's CIVET_DIST_DIR).
#
# Requirements: zig (brew install zig / your distro's package) and
#   cargo install --locked cargo-zigbuild
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
# Uses the `dist` cargo profile (release + opt-level "z", thin LTO, stripped).
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

out=target/civet-dist
install=0
targets=()
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --install) install=1; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) targets+=("$1"); shift ;;
    esac
done
[ ${#targets[@]} -eq 0 ] && targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)

command -v cargo-zigbuild >/dev/null || { echo "cargo-zigbuild not found: cargo install --locked cargo-zigbuild" >&2; exit 1; }
command -v zig >/dev/null || { echo "zig not found (brew install zig)" >&2; exit 1; }

mkdir -p "$out"
for t in "${targets[@]}"; do
    rustup target list --installed | grep -qx "$t" || rustup target add "$t"
    echo "==> civetd for $t"
    cargo zigbuild -p civetd --bin civetd --profile dist --target "$t"
    cp "target/$t/dist/civetd" "$out/civetd-$t"
    chmod 755 "$out/civetd-$t"
    ls -l "$out/civetd-$t"
    if command -v file >/dev/null; then file "$out/civetd-$t"; fi
done

if [ "$install" = 1 ]; then
    mkdir -p "$HOME/.civet/dist"
    cp "$out"/civetd-* "$HOME/.civet/dist/"
    echo "installed to ~/.civet/dist"
fi
echo
echo "export CIVET_DIST_DIR=$(cd "$out" && pwd)"
