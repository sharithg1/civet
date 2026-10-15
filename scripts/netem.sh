#!/usr/bin/env bash
# Linux network impairment for real-host Civet testing (tc netem + iptables).
# Run on the client or the server host (or a router in between) as root.
#
#   netem.sh apply   IFACE [--loss 5%] [--delay 50ms] [--jitter 10ms] [--reorder 2%] [--duplicate 0%]
#   netem.sh clear   IFACE
#   netem.sh show    IFACE
#   netem.sh blackhole [--port 60001:60999] SECS     drop Civet UDP both ways for SECS ("sleep")
#   netem.sh block-udp [--port 60001:60999]          drop Civet UDP until unblock-udp (fallback test)
#   netem.sh unblock-udp [--port 60001:60999]
#   netem.sh roam IFACE [--port 60001:60999] [--every 10] [--count 6]
#                       NAT rebinding: SNAT Civet UDP to a random source port and
#                       forget the mapping every N seconds, so the daemon sees the
#                       client at a new address (run on the CLIENT host or a router)
#   netem.sh profile IFACE wifi|lossy|terrible|satellite
#
# Notes
# * `apply` shapes egress of IFACE only: apply on both hosts (or on a router)
#   for symmetric impairment; RTT adds up from both directions.
# * The daemon's UDP ports are 60001-60999 unless CIVET_PORT_RANGE says
#   otherwise; the client's source port is ephemeral, so rules match on the
#   daemon port as destination (outbound) or source (inbound).
# * Requirements: iproute2 (tc), iptables, conntrack-tools (for `roam`).
#
# Example: a full hardening pass against a dev box
#   sudo scripts/netem.sh profile eth0 lossy        # 5% loss, 100 ms RTT
#   civet-cli --ssh devbox connect                   # type, cat, run an agent
#   sudo scripts/netem.sh roam eth0 --every 5 &      # roaming while typing
#   sudo scripts/netem.sh blackhole 60               # a 60 s "sleep"
#   sudo scripts/netem.sh block-udp; civet-cli --ssh devbox connect   # falls back to ssh
#   sudo scripts/netem.sh unblock-udp; sudo scripts/netem.sh clear eth0
set -euo pipefail

die() { echo "netem.sh: $*" >&2; exit 2; }
need() { command -v "$1" >/dev/null || die "$1 not found"; }
[ "$(uname -s)" = Linux ] || die "Linux only (tc netem); on macOS use the in-test proxy (cargo test -p civet-e2e)"
[ "$(id -u)" = 0 ] || die "run as root"

port="60001:60999"
cmd="${1:-}"; [ -n "$cmd" ] || { sed -n '2,32p' "$0"; exit 2; }
shift

ipt_rules() { # $1 = -I|-D
    iptables "$1" OUTPUT -p udp --dport "$port" -j DROP
    iptables "$1" INPUT -p udp --sport "$port" -j DROP
    iptables "$1" INPUT -p udp --dport "$port" -j DROP
    iptables "$1" OUTPUT -p udp --sport "$port" -j DROP
}

case "$cmd" in
apply)
    need tc
    iface="${1:-}"; [ -n "$iface" ] || die "apply IFACE ..."; shift
    loss=0%; delay=0ms; jitter=0ms; reorder=0%; dup=0%
    while [ $# -gt 0 ]; do
        case "$1" in
            --loss) loss="$2"; shift 2 ;;
            --delay) delay="$2"; shift 2 ;;
            --jitter) jitter="$2"; shift 2 ;;
            --reorder) reorder="$2"; shift 2 ;;
            --duplicate) dup="$2"; shift 2 ;;
            *) die "unknown option $1" ;;
        esac
    done
    # reorder needs a delay to have any effect (netem sends reordered packets immediately).
    tc qdisc replace dev "$iface" root netem delay "$delay" "$jitter" distribution normal \
        loss "$loss" reorder "$reorder" duplicate "$dup"
    tc qdisc show dev "$iface"
    ;;
clear)
    need tc
    tc qdisc del dev "${1:?clear IFACE}" root 2>/dev/null || true
    tc qdisc show dev "$1"
    ;;
show)
    need tc
    tc -s qdisc show dev "${1:?show IFACE}"
    ;;
profile)
    iface="${1:?profile IFACE NAME}"; name="${2:?profile IFACE NAME}"
    case "$name" in
        wifi)      "$0" apply "$iface" --delay 15ms --jitter 5ms --loss 0.5% ;;
        lossy)     "$0" apply "$iface" --delay 50ms --jitter 10ms --loss 5% --reorder 2% ;;
        terrible)  "$0" apply "$iface" --delay 50ms --jitter 20ms --loss 20% --reorder 5% ;;
        satellite) "$0" apply "$iface" --delay 300ms --jitter 30ms --loss 1% ;;
        *) die "profiles: wifi lossy terrible satellite" ;;
    esac
    ;;
blackhole)
    need iptables
    [ "${1:-}" = --port ] && { port="$2"; shift 2; }
    secs="${1:?blackhole SECS}"
    trap 'ipt_rules -D 2>/dev/null || true' EXIT
    ipt_rules -I
    echo "$(date +%T.%N) Civet UDP ($port) blackholed for ${secs}s"
    sleep "$secs"
    ipt_rules -D
    trap - EXIT
    echo "$(date +%T.%N) restored"
    ;;
block-udp)
    need iptables
    [ "${1:-}" = --port ] && port="$2"
    ipt_rules -I
    echo "Civet UDP ($port) blocked; undo with: $0 unblock-udp"
    ;;
unblock-udp)
    need iptables
    [ "${1:-}" = --port ] && port="$2"
    ipt_rules -D 2>/dev/null || true
    echo "Civet UDP ($port) unblocked"
    ;;
roam)
    need iptables; need conntrack
    iface="${1:?roam IFACE}"; shift
    every=10; count=6
    while [ $# -gt 0 ]; do
        case "$1" in
            --port) port="$2"; shift 2 ;;
            --every) every="$2"; shift 2 ;;
            --count) count="$2"; shift 2 ;;
            *) die "unknown option $1" ;;
        esac
    done
    rule=(POSTROUTING -o "$iface" -p udp --dport "$port" -j MASQUERADE --to-ports 20000-40000 --random)
    trap 'iptables -t nat -D "${rule[@]}" 2>/dev/null || true' EXIT
    iptables -t nat -I "${rule[@]}"
    for i in $(seq 1 "$count"); do
        sleep "$every"
        # Forget the NAT mapping: the next packet gets a new random source port.
        conntrack -D -p udp --dport "${port%%:*}" >/dev/null 2>&1 || true
        if [ "${port#*:}" != "$port" ]; then
            conntrack -D -p udp --orig-port-dst "${port%%:*}" >/dev/null 2>&1 || true
            # Ranges: flush every UDP mapping towards the range.
            conntrack -L -p udp 2>/dev/null | awk -v lo="${port%%:*}" -v hi="${port##*:}" '
                { for (i = 1; i <= NF; i++) if ($i ~ /^dport=/) { split($i, a, "="); if (a[2] >= lo && a[2] <= hi) print $0; break } }' \
                | sed -n 's/.*src=\([^ ]*\) dst=\([^ ]*\) sport=\([0-9]*\) dport=\([0-9]*\).*/\1 \2 \3 \4/p' \
                | while read -r s d sp dp; do conntrack -D -p udp -s "$s" -d "$d" --sport "$sp" --dport "$dp" >/dev/null 2>&1 || true; done
        fi
        echo "$(date +%T.%N) roam $i/$count: NAT mapping reset"
    done
    ;;
*)
    die "unknown command $cmd (see $0 with no arguments)"
    ;;
esac
