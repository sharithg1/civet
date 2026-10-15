#!/usr/bin/env bash
# Bump the version, commit "Release vX.Y.Z" and create the annotated tag
# vX.Y.Z. Never pushes: it prints the push command, and pushing the tag
# starts .github/workflows/release.yml.
#
#   scripts/release.sh 0.1.5              # on a clean, up-to-date main
#   scripts/release.sh 0.1.5 --dry-run    # show the diff, then undo it (no commit, no tag)
#   scripts/release.sh 0.1.5 --allow-branch   # release from a branch other than main
#
# Updates: root Cargo.toml ([workspace.package] version, inherited by every
# crate), crates/civet-desktop/tauri.conf.json ("version") and Cargo.lock.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

usage() { sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

version=""
dry_run=0
allow_branch=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run|-n) dry_run=1 ;;
        --allow-branch) allow_branch=1 ;;
        -h|--help) usage ;;
        -*) echo "unknown option: $1" >&2; usage 1 ;;
        *) [ -z "$version" ] || usage 1; version=${1#v} ;;
    esac
    shift
done
[ -n "$version" ] || usage 1
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "error: '$version' is not a semver version like 0.1.5 or 0.2.0-rc.1" >&2
    exit 1
fi
tag="v$version"

die() { echo "error: $*" >&2; exit 1; }

# --- preconditions ---------------------------------------------------------
current=$(scripts/check-version.sh) || die "versions disagree before the bump; fix that first"
[ "$current" != "$version" ] || die "already at $version"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"
branch=$(git symbolic-ref --quiet --short HEAD) || die "detached HEAD"
if [ "$branch" != main ] && [ "$allow_branch" = 0 ]; then
    die "on branch '$branch', not main (pass --allow-branch to release from it anyway)"
fi
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists locally"
if remote_tags=$(git ls-remote --tags origin "refs/tags/$tag" 2>/dev/null); then
    [ -z "$remote_tags" ] || die "tag $tag already exists on origin"
else
    echo "warning: could not reach origin to check for an existing $tag" >&2
fi
if [ "$branch" = main ] && git rev-parse -q --verify origin/main >/dev/null; then
    git fetch -q origin main 2>/dev/null || echo "warning: could not fetch origin/main" >&2
    [ "$(git rev-list --count HEAD..origin/main)" = 0 ] || die "main is behind origin/main; pull first"
fi

# --- bump --------------------------------------------------------------------
echo "==> $current -> $version"
# Cargo.toml: only the version line inside [workspace.package].
awk -v v="$version" '
    /^\[/ { in_pkg = ($0 == "[workspace.package]") }
    in_pkg && !done && /^version[ \t]*=/ { print "version = \"" v "\""; done = 1; next }
    { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml
# tauri.conf.json: the top-level "version" key (two-space indent), first match only.
awk -v v="$version" '
    !done && /^  "version": *"[^"]*"/ { sub(/"version": *"[^"]*"/, "\"version\": \"" v "\""); done = 1 }
    { print }
' crates/civet-desktop/tauri.conf.json > crates/civet-desktop/tauri.conf.json.tmp \
    && mv crates/civet-desktop/tauri.conf.json.tmp crates/civet-desktop/tauri.conf.json
# Cargo.lock: rewrite the workspace members' versions only (no dependency updates).
cargo update --workspace --offline -q 2>/dev/null || cargo update --workspace -q

undo() { git checkout -q -- Cargo.toml Cargo.lock crates/civet-desktop/tauri.conf.json; }
if ! scripts/check-version.sh "$tag" >/dev/null; then
    undo
    die "version bump did not apply cleanly; reverted"
fi
git --no-pager diff --stat
git --no-pager diff -U0 -- Cargo.toml crates/civet-desktop/tauri.conf.json

if [ "$dry_run" = 1 ]; then
    undo
    echo
    echo "dry run: reverted; would commit \"Release $tag\" and tag $tag on $branch"
    exit 0
fi

# --- commit + tag --------------------------------------------------------------
git add Cargo.toml Cargo.lock crates/civet-desktop/tauri.conf.json
git commit -q -m "Release $tag"
git tag -a "$tag" -m "Release $tag"
echo
echo "Committed \"Release $tag\" ($(git rev-parse --short HEAD)) and tagged $tag. Nothing was pushed."
echo "Push both to start the release workflow:"
echo
echo "    git push --atomic origin $branch $tag"
echo
echo "To undo before pushing:  git tag -d $tag && git reset --hard HEAD~1"
