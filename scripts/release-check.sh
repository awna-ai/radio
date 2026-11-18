#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
usage() {
    printf 'usage: scripts/release-check.sh [--remote <name>] <commit>\n' >&2
    exit 2
}
fail() {
    printf 'release-check: %s\n' "$1" >&2
    exit 1
}
remote=origin
if [ "${1:-}" = --remote ]; then
    [ "$#" -ge 2 ] || usage
    remote=$2
    case "$remote" in ''|-*) usage ;; esac
    shift 2
fi
[ "$#" -eq 1 ] || usage
case "$1" in -*) usage ;; esac
urls=$(git remote get-url --all -- "$remote" 2> /dev/null) || usage
push_urls=$(git remote get-url --push --all -- "$remote" 2> /dev/null) || usage
[[ "$urls" != *$'\n'* ]] && [ "$urls" = "$push_urls" ] || usage
commit=$(git rev-parse --verify --quiet --end-of-options "$1^{commit}") || {
    printf 'release-check: %s is not a commit\n' "$1" >&2
    exit 2
}
refs=$(git ls-remote -- "$remote" refs/heads/main refs/green/main refs/green/frozen) || fail "could not read $remote"
main=''
green=''
frozen=''
while read -r oid ref; do
    [ -n "$oid" ] || continue
    [[ "$oid" =~ ^[0-9a-fA-F]{40}$|^[0-9a-fA-F]{64}$ ]] || fail "invalid oid from $remote"
    case "$ref" in
        refs/heads/main) main=$oid ;;
        refs/green/main) green=$oid ;;
        refs/green/frozen) frozen=$oid ;;
    esac
done <<< "$refs"
[ -z "$frozen" ] || fail "releases are frozen at ${frozen:0:12}: main went red twice there. Revert or fix it, and wait for the next green."
[ -n "$green" ] || fail 'nothing on main has passed the full check yet.'
[ -n "$main" ] || fail "$remote has no main."
if ! git cat-file -e "$green^{commit}" 2> /dev/null || ! git cat-file -e "$main^{commit}" 2> /dev/null; then
    git fetch --quiet -- "$remote" main || fail "could not fetch $remote"
    git cat-file -e "$green^{commit}" 2> /dev/null || fail "could not fetch $remote"
    git cat-file -e "$main^{commit}" 2> /dev/null || fail "could not fetch $remote"
fi
git merge-base --is-ancestor "$green" "$main" || fail "green ${green:0:12} is not on $remote/main; move it back onto main by hand (git push $remote +<a main commit>:refs/green/main)."
if git merge-base --is-ancestor "$commit" "$green"; then
    printf 'release-check: %s passed the full check (green is %s).\n' "${commit:0:12}" "${green:0:12}"
    exit 0
fi
fail "${commit:0:12} has not passed the full check on main yet (green is ${green:0:12}). Release a green commit, or run scripts/verify-main.sh first."
