#!/usr/bin/env bash
set -euo pipefail

# Exit codes: 0 green, current, covered or already running; 1 failed (nothing
# judged, nothing changed); 2 usage; 5 red with one culprit; 6 red across several commits.
root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
usage() {
    printf 'usage: scripts/verify-main.sh [--remote <name>] [-- <check command>...]\n' >&2
    exit 2
}
fail() {
    printf 'verify-main: %s\n' "$1" >&2
    exit 1
}
remote=origin
check=(env RADIO_STRICT=1 bash scripts/ci-local.sh)
while [ "$#" -gt 0 ]; do
    case "$1" in
        --remote)
            [ "$#" -ge 2 ] || usage
            remote=$2
            case "$remote" in ''|-*) usage ;; esac
            shift 2 ;;
        --)
            shift
            [ "$#" -gt 0 ] || usage
            check=("$@")
            break ;;
        *) usage ;;
    esac
done
git remote get-url -- "$remote" > /dev/null 2>&1 || usage
common=$(git rev-parse --git-common-dir) || fail 'could not find the common git directory'
lock="$common/verify-main.lock"
work=''
cleanup() {
    if [ -n "$work" ]; then
        git worktree remove --force "$work/wt" > /dev/null 2>&1 || true
        rm -rf "$work"
        git worktree prune > /dev/null 2>&1 || true
    fi
    if [ "$(readlink "$lock" 2> /dev/null || true)" = "$$" ]; then
        rm -f "$lock"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
running() {
    local pid
    pid=$(readlink "$lock" 2> /dev/null || true)
    case "$pid" in ''|*[!0-9]*) pid=unknown ;; esac
    printf 'verify-main: already running (pid %s)\n' "$pid"
    exit 0
}
if ! ln -s "$$" "$lock" 2> /dev/null; then
    owner=$(readlink "$lock" 2> /dev/null || true)
    case "$owner" in
        ''|*[!0-9]*) ;;
        *) if kill -0 "$owner" 2> /dev/null; then running; fi ;;
    esac
    if [ "$(readlink "$lock" 2> /dev/null || true)" = "$owner" ]; then
        rm -f "$lock" || running
    fi
    ln -s "$$" "$lock" 2> /dev/null || running
fi
fetch_main() {
    git fetch --quiet -- "$remote" main || fail "could not fetch $remote"
}
read_refs() {
    local refs oid ref
    refs=$(git ls-remote -- "$remote" refs/green/main refs/green/frozen) || fail "could not read $remote"
    green=''
    frozen=''
    while read -r oid ref; do
        [ -n "$oid" ] || continue
        [[ "$oid" =~ ^[0-9a-fA-F]{40}$|^[0-9a-fA-F]{64}$ ]] || fail "invalid oid from $remote"
        case "$ref" in
            refs/green/main) green=$oid ;;
            refs/green/frozen) frozen=$oid ;;
        esac
    done <<< "$refs"
}
ensure_commit() {
    if ! git cat-file -e "$1^{commit}" 2> /dev/null; then
        git fetch --quiet -- "$remote" "$1" || fail "could not fetch $remote"
        git cat-file -e "$1^{commit}" 2> /dev/null || fail "could not fetch $remote"
    fi
}
fetch_main
head=$(git rev-parse --verify "refs/remotes/$remote/main^{commit}") || fail "could not fetch $remote"
read_refs
if [ "$green" = "$head" ]; then
    printf 'green is current: %s\n' "$head"
    exit 0
fi
if [ -n "$green" ]; then
    ensure_commit "$green"
    git merge-base --is-ancestor "$green" "$head" || fail "green ${green:0:12} is not on $remote/main; nothing changed"
    commits=$(git rev-list --first-parent --reverse "$green..$head")
else
    commits=$(git rev-list --first-parent --reverse "$head")
fi
count=$(printf '%s\n' "$commits" | wc -l | tr -d ' ')
first=${commits%%$'\n'*}
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
git worktree add --quiet --detach "$work/wt" "$head" || fail 'could not create the scratch worktree'
run_check() {
    if [ -n "$green" ]; then
        printf 'verify-main: checking %s (%s commits since green %s)\n' "${head:0:12}" "$count" "${green:0:12}"
    else
        printf 'verify-main: checking %s (no green yet)\n' "${head:0:12}"
    fi
    # Keep the check in its own process so failures retain normal errexit semantics.
    (cd "$work/wt" && exec "${check[@]}")
}
status=0
run_check || status=$?
if [ "$status" -ne 0 ]; then
    printf 'verify-main: %s was red (exit %s); running it once more\n' "${head:0:12}" "$status"
    status=0
    run_check || status=$?
    if [ "$status" -eq 0 ]; then
        printf 'verify-main: %s passed on the rerun (the first run was red)\n' "${head:0:12}"
    fi
fi
red_line() {
    if [ "$count" -eq 1 ]; then
        printf 'red %s culprit %s\n' "$head" "$first"
    else
        printf 'red %s across %s commits from %s to %s\n' "$head" "$count" "$first" "$head"
    fi
}
if [ "$status" -ne 0 ]; then
    if ! git push --porcelain "--force-with-lease=refs/green/frozen:$frozen" -- "$remote" "$head:refs/green/frozen"; then
        red_line
        fail "${head:0:12} is red, and the freeze could not be pushed"
    fi
    if [ "$count" -eq 1 ]; then
        printf 'verify-main: releases are frozen. Revert or fix %s, then run this again.\n' "${first:0:12}"
        red_line
        exit 5
    fi
    printf 'verify-main: releases are frozen. One of the %s commits from %s to %s broke main; revert or fix it, then run this again.\n' "$count" "${first:0:12}" "${head:0:12}"
    red_line
    exit 6
fi
fetch_main
new_head=$(git rev-parse --verify "refs/remotes/$remote/main^{commit}") || fail "could not fetch $remote"
git merge-base --is-ancestor "$head" "$new_head" || fail "$remote/main no longer holds ${head:0:12}; nothing changed"
leases=("--force-with-lease=refs/green/main:$green")
refs=("$head:refs/green/main")
cleared=''
if [ -n "$frozen" ]; then
    ensure_commit "$frozen"
    if git merge-base --is-ancestor "$frozen" "$head"; then
        leases+=("--force-with-lease=refs/green/frozen:$frozen")
        refs+=(:refs/green/frozen)
        cleared=$frozen
    fi
fi
push_status=0
pushed=$(git push --atomic --porcelain "${leases[@]}" -- "$remote" "${refs[@]}") || push_status=$?
# Git can report '=' even when the lease no longer matched.
if [ "$push_status" -ne 0 ] || [[ "$pushed" == *$'\n='* ]] || [[ "$pushed" == =* ]]; then
    read_refs
    if [ -n "$green" ]; then
        ensure_commit "$green"
        if git merge-base --is-ancestor "$head" "$green"; then
            printf 'covered: green is %s\n' "$green"
            exit 0
        fi
    fi
    fail 'green moved during the check; nothing changed; run it again'
fi
printf 'green %s\n' "$head"
if [ -n "$cleared" ]; then
    printf 'releases unfrozen (freeze at %s cleared)\n' "$cleared"
fi
