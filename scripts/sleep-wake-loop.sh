#!/usr/bin/env bash
# macOS sleep/wake loop for the "lid close/open" success metric.
# DOCUMENTATION / MANUAL USE ONLY: this puts the Mac to sleep repeatedly.
#
#   sudo scripts/sleep-wake-loop.sh [CYCLES=10] [SLEEP_SECS=60] [AWAKE_SECS=20]
#
# Setup (two terminals):
#   1. Attach a pane that prints a timestamp every 100 ms, with resume logging:
#        civet-cli --ssh devbox -v connect -- sh -c \
#          'i=0; while :; do i=$((i+1)); echo "$i $(date +%T)"; sleep 0.1; done' \
#          2> /tmp/civet-attach.log
#      (or use the desktop app; its log shows host-state transitions).
#   2. In the other terminal: sudo scripts/sleep-wake-loop.sh 10 60
#
# Each cycle schedules a wake with `pmset schedule wake`, sleeps with
# `pmset sleepnow`, and after waking records wall-clock wake time and the
# time until a fresh `civet-cli ... ping` succeeds (bootstrap + QUIC; an
# upper bound, the attached session resumes without any handshake).
# Results: /tmp/civet-sleep-wake.tsv.
#
# Afterwards check, in the attached pane:
#   * the counter has no gaps and no repeats (scroll up / civet-cli scrollback PANE);
#   * on Ctrl-] civet-cli prints reconnects / resumes / longest Recovering
#     duration: target "network back -> live" < 200 ms, 100% of cycles;
#   * /tmp/civet-attach.log (with -v) has a "recovering (reason)" line per
#     wake that needed recovery.
# civet-cli has no wake detector (the desktop app has one and calls
# notify_network_change on wake), so it notices a dead path by its 1 s
# heartbeat + 1 s stall timer: its numbers are an upper bound for the app.
# Notes: pmset needs root; a scheduled wake can be up to a few seconds late;
# Power Nap / "wake for network access" can keep the network up during sleep
# (disable them for a real test: sudo pmset -a powernap 0 womp 0, restore after).
set -euo pipefail

cycles="${1:-10}"
sleep_secs="${2:-60}"
awake_secs="${3:-20}"
dest="${CIVET_SSH_DEST:-devbox}"
cli="${CIVET_CLI:-civet-cli}"
out=/tmp/civet-sleep-wake.tsv

[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { echo "run with sudo (pmset)" >&2; exit 2; }
user="${SUDO_USER:-$USER}"

printf 'cycle\tslept_at\twoke_at\tping_ok_ms\n' > "$out"
for i in $(seq 1 "$cycles"); do
    wake_at=$(date -v+"${sleep_secs}"S '+%m/%d/%y %H:%M:%S')
    pmset schedule wake "$wake_at"
    slept=$(date +%s)
    echo "cycle $i/$cycles: sleeping until $wake_at"
    pmset sleepnow >/dev/null
    # Execution continues after wake; wait until the clock passed the wake time.
    while [ $(( $(date +%s) - slept )) -lt "$sleep_secs" ]; do sleep 0.2; done
    woke=$(date +%s)
    t0=$(python3 -c 'import time; print(time.time())')
    until sudo -u "$user" "$cli" --ssh "$dest" ping --count 1 >/dev/null 2>&1; do sleep 0.05; done
    t1=$(python3 -c 'import time; print(time.time())')
    ms=$(python3 -c "print(round(($t1 - $t0) * 1000))")
    printf '%s\t%s\t%s\t%s\n' "$i" "$slept" "$woke" "$ms" >> "$out"
    echo "cycle $i: awake; fresh civet-cli ping ok after ${ms} ms"
    sleep "$awake_secs"
done
column -t "$out"
