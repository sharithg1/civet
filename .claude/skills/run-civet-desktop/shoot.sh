#!/usr/bin/env bash
# Run tour.txt against the mock UI at each window size, at 2x scale.
# With --base <ref>, also run it against <ref> (a temporary git worktree) and
# write before/after/diff composites of every screenshot that changed.
#
#   shoot.sh <out-dir> [--base <ref>] [--sizes "1400x900 1100x720"] [--tour file]
#
# Output: <out>/after/<size>/*.png, and with --base also <out>/before/<size>/
# and <out>/compare/<size>/. Ports: PORT (default 5231), BASE_PORT (5232).
set -euo pipefail
export PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH"

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
out="${1:?usage: shoot.sh <out-dir> [--base <ref>] [--sizes \"WxH ...\"] [--tour file]}"
shift
base=""
sizes="1400x900 1100x720"
tour="$here/tour.txt"
while [ $# -gt 0 ]; do
  case "$1" in
    --base) base="$2"; shift 2 ;;
    --sizes) sizes="$2"; shift 2 ;;
    --tour) tour="$2"; shift 2 ;;
    *) echo "unknown option $1"; exit 2 ;;
  esac
done
mkdir -p "$out"
out="$(cd "$out" && pwd)"
port="${PORT:-5231}"
base_port="${BASE_PORT:-5232}"
pids=()
base_src=""
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  if [ -n "$base_src" ]; then git -C "$repo" worktree remove --force "$base_src" 2>/dev/null || true; fi
}
trap cleanup EXIT

# Start Vite for the UI under $1 on port $2 and wait until it answers.
serve() {
  if curl -sf -o /dev/null "localhost:$2"; then
    echo "port $2 is already taken (another Vite?); set PORT / BASE_PORT" >&2
    exit 1
  fi
  (cd "$1/crates/civet-desktop/ui" && exec pnpm exec vite --port "$2" --strictPort >"$out/vite-$2.log" 2>&1) &
  pids+=($!)
  curl -sf --retry 40 --retry-connrefused --retry-delay 1 -o /dev/null "localhost:$2" ||
    { echo "Vite on $2 never came up; see $out/vite-$2.log" >&2; exit 1; }
}

# Run the tour against port $1 into $2/<size>/.
tour_all() {
  for s in $sizes; do
    echo "== $2 $s"
    KEEP_GOING=1 VIEWPORT="$s" SHOTS="$out/$2/$s" node "$here/driver.mjs" "http://localhost:$1/?mock" <"$tour" |
      grep -v '^screenshot' || true
  done
}

serve "$repo" "$port"
tour_all "$port" after

if [ -n "$base" ]; then
  base_src="$out/.base-src"
  rm -rf "$out/before" "$out/compare"
  git -C "$repo" worktree add --detach --force "$base_src" "$base" >/dev/null
  (cd "$base_src/crates/civet-desktop/ui" && pnpm install --frozen-lockfile --prefer-offline >/dev/null)
  serve "$base_src" "$base_port"
  tour_all "$base_port" before
  for s in $sizes; do
    echo "== compare $s ($base -> working tree)"
    node "$here/compare.mjs" "$out/before/$s" "$out/after/$s" "$out/compare/$s"
  done
fi
echo "shots in $out"
