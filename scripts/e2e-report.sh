#!/usr/bin/env bash
# Run the civet-e2e hardening suite (including the long #[ignore]d tests)
# in release mode and print every measured number.
#
#   scripts/e2e-report.sh                 # everything, default durations
#   scripts/e2e-report.sh --quick         # skip the #[ignore]d long tests
#   CIVET_E2E_SLEEP_SECS=60 scripts/e2e-report.sh
#
# Knobs (environment): CIVET_E2E_RUNS (network-change runs, 20),
# CIVET_E2E_RTTS_MS ("0,100"), CIVET_E2E_SLEEP_SECS (10), CIVET_E2E_SOAK_SECS
# (60), CIVET_E2E_SEED (proxy RNG, 42), CIVET_E2E_LOG (tracing filter).
#
# Metrics go to target/e2e-metrics.jsonl (one JSON object per line) and are
# summarized at the end. Every test uses temp state dirs; no daemon is left
# running afterwards (checked below).
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

mode=(-- --include-ignored)
if [ "${1:-}" = "--quick" ]; then
    mode=(--)
fi

out=target/e2e-metrics.jsonl
mkdir -p target
: > "$out"
export CIVET_E2E_METRICS="$PWD/$out"

# Build the daemon binary up front so no test measures a build.
cargo build --release -p civetd --bin civetd
export CIVETD_BIN="$PWD/target/release/civetd"

status=0
cargo test -p civet-e2e --release "${mode[@]}" --nocapture --test-threads=1 2>&1 \
    | grep -v '^METRIC ' || status=$?

echo
echo "== metrics ($out) =="
if command -v jq >/dev/null 2>&1; then
    jq -r '"\(.metric)\t\(.value) \(.unit)"' "$out" | column -t -s $'\t'
else
    cat "$out"
fi

# Leftover daemons from this checkout would be a bug in the harness.
if pgrep -fl "$PWD/target/.*/civetd" | grep -v -- "--state-dir $HOME/.civet" ; then
    echo "WARNING: civetd processes from this checkout are still running" >&2
    status=1
fi
exit "$status"
