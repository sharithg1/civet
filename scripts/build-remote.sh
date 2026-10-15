#!/usr/bin/env bash
# Build static civetd binaries for Linux remote hosts with cargo-zigbuild
# (and, with --bundle, this Mac's civetd for the desktop app's "Local" host).
#
#   scripts/build-remote.sh                      # both Linux targets -> target/civet-dist/
#   scripts/build-remote.sh x86_64-unknown-linux-musl
#   scripts/build-remote.sh --out DIR            # put civetd-<triple> files in DIR
#   scripts/build-remote.sh --install            # also copy to ~/.civet/dist
#   scripts/build-remote.sh --bundle             # Linux targets + this Mac's triple into
#                                                # crates/civet-desktop/civet-dist (the app's
#                                                # bundled resources; `tauri build` runs this)
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
# Darwin targets build natively with cargo (no zig).
#
# CIVET_SKIP_DAEMONS=1 makes --bundle a no-op: UI-only builds (the app then
# finds daemons in $CIVET_DIST_DIR / ~/.civet/dist only), or release CI,
# which fills crates/civet-desktop/civet-dist itself before `tauri build`.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

out=target/civet-dist
install=0
bundle=0
targets=()
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --install) install=1; shift ;;
        --bundle) bundle=1; shift ;;
        -h|--help) sed -n '2,27p' "$0"; exit 0 ;;
        *) targets+=("$1"); shift ;;
    esac
done
host=$(rustc -vV | sed -n 's/^host: //p')
if [ "$bundle" = 1 ]; then
    if [ "${CIVET_SKIP_DAEMONS:-0}" = 1 ]; then
        echo "CIVET_SKIP_DAEMONS=1: not bundling daemons"
        exit 0
    fi
    out=crates/civet-desktop/civet-dist
    if [ ${#targets[@]} -eq 0 ]; then
        targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
        # The app's own triple: `tauri build --target T` exports
        # TAURI_ENV_TARGET_TRIPLE to this (beforeBuildCommand) script, so a
        # cross build bundles the daemon for the Mac it will run on.
        app_triple=${TAURI_ENV_TARGET_TRIPLE:-$host}
        case "$app_triple" in
            universal-apple-darwin) targets+=(aarch64-apple-darwin x86_64-apple-darwin) ;;
            *-apple-darwin) targets+=("$app_triple") ;;
        esac
    fi
fi
[ ${#targets[@]} -eq 0 ] && targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)

needs_zig=0
for t in "${targets[@]}"; do
    case "$t" in *-linux-*) needs_zig=1 ;; esac
done
if [ "$needs_zig" = 1 ]; then
    command -v cargo-zigbuild >/dev/null || { echo "cargo-zigbuild not found: cargo install --locked cargo-zigbuild" >&2; exit 1; }
    command -v zig >/dev/null || { echo "zig not found (brew install zig)" >&2; exit 1; }
fi

mkdir -p "$out"
for t in "${targets[@]}"; do
    rustup target list --installed | grep -qx "$t" || rustup target add "$t"
    echo "==> civetd for $t"
    case "$t" in
        *-apple-darwin) cargo build -p civetd --bin civetd --profile dist --target "$t" ;;
        *) cargo zigbuild -p civetd --bin civetd --profile dist --target "$t" ;;
    esac
    # Replace atomically (a running local daemon may execute the old file).
    cp "target/$t/dist/civetd" "$out/.civetd-$t.tmp"
    chmod 755 "$out/.civetd-$t.tmp"
    mv -f "$out/.civetd-$t.tmp" "$out/civetd-$t"
    ls -l "$out/civetd-$t"
    if command -v file >/dev/null; then file "$out/civetd-$t"; fi
done

if [ "$install" = 1 ]; then
    mkdir -p "$HOME/.civet/dist"
    for t in "${targets[@]}"; do
        cp "$out/civetd-$t" "$HOME/.civet/dist/.civetd-$t.tmp"
        mv -f "$HOME/.civet/dist/.civetd-$t.tmp" "$HOME/.civet/dist/civetd-$t"
    done
    echo "installed to ~/.civet/dist"
fi
echo
echo "export CIVET_DIST_DIR=$(cd "$out" && pwd)"
