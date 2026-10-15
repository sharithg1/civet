#!/usr/bin/env bash
# Start a local civetd in a throwaway state dir and attach civet-cli to a
# new shell pane, for manual testing. Everything stops when you detach
# (Ctrl-]) or the script exits.
#
#   scripts/dev-local.sh                 # debug build, QUIC, new shell pane
#   scripts/dev-local.sh --release
#   scripts/dev-local.sh --stdio         # the UDP-blocked fallback path (stdio tunnel)
#   scripts/dev-local.sh --rebind 2      # rebind the client's UDP socket every 2 s
#   scripts/dev-local.sh -- htop         # run a command instead of a shell
#   CIVET_STATE_DIR=/tmp/civet-dev scripts/dev-local.sh   # keep/reuse a state dir
#
# Useful while it runs (other terminal, same state dir; printed below):
#   civet-cli --local --state-dir DIR panes | events | ping --count 20
#   civet-cli --local --state-dir DIR scrollback 1 --limit 50
#   tail -f DIR/logs/civetd.log
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

profile=debug
cargo_flags=()
cli_flags=()
cmd=()
while [ $# -gt 0 ]; do
    case "$1" in
        --release) profile=release; cargo_flags+=(--release); shift ;;
        --stdio) cli_flags+=(--stdio); shift ;;
        --rebind) cli_flags+=(--rebind-every "$2" -v); shift 2 ;;
        --) shift; cmd=("$@"); break ;;
        -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

cargo build ${cargo_flags[@]+"${cargo_flags[@]}"} -p civetd --bins
bin="target/$profile"

keep=1
if [ -z "${CIVET_STATE_DIR:-}" ]; then
    CIVET_STATE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/civet-dev.XXXXXX")"
    keep=0
fi
export CIVET_STATE_DIR
export CIVET_LOG="${CIVET_LOG:-info}"

stop() {
    if [ -f "$CIVET_STATE_DIR/daemon.json" ]; then
        pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$CIVET_STATE_DIR/daemon.json")
        [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    fi
    [ "$keep" = 0 ] && rm -rf "$CIVET_STATE_DIR"
}
trap stop EXIT

# Detached daemon (logs in $CIVET_STATE_DIR/logs/civetd.log), as a remote
# host would run it after `civetd bootstrap`.
"$bin/civetd" bootstrap --state-dir "$CIVET_STATE_DIR" >/dev/null
echo "civetd running: state dir $CIVET_STATE_DIR"
echo "  $bin/civet-cli --local --civetd $bin/civetd --state-dir $CIVET_STATE_DIR panes"
echo "Attaching (Ctrl-] detaches and stops the daemon)..."

"$bin/civet-cli" --local --civetd "$bin/civetd" --state-dir "$CIVET_STATE_DIR" \
    ${cli_flags[@]+"${cli_flags[@]}"} connect --cwd "$PWD" -- ${cmd[@]+"${cmd[@]}"}
