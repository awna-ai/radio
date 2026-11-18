#!/usr/bin/env bash
set -u -o pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
tmp=$(mktemp -d)
tmp=$(cd "$tmp" && pwd -P)
failed=0
sleeper=''
cleanup() {
    [ -z "$sleeper" ] || { kill "$sleeper" 2> /dev/null; wait "$sleeper" 2> /dev/null; }
    rm -rf "$tmp"
}
trap cleanup EXIT
fail() {
    printf 'verify_main_test: FAIL: %s\n' "$1" >&2
    failed=1
}
for script in verify-main release-check; do
    [ -f "$root/scripts/$script.sh" ] || { fail "scripts/$script.sh is missing"; exit 1; }
done
# No network: every remote is a bare repository in this temporary folder.
git init --bare -q -b main "$tmp/remote.git" || exit 1
git clone -q "$tmp/remote.git" "$tmp/work" 2> /dev/null || exit 1
work="$tmp/work"
mkdir "$work/scripts"
cp "$root/scripts/verify-main.sh" "$root/scripts/release-check.sh" "$work/scripts/"
git -C "$work" config user.name t
git -C "$work" config user.email t@example.invalid
git -C "$work" config commit.gpgsign false
git -C "$work" config core.hooksPath /dev/null
git -C "$tmp/remote.git" config core.hooksPath /dev/null
push_commit() {
    local message=$1
    shift
    case "${1:-}" in
        add) printf 'red\n' > "$work/$2" ;;
        rm) rm "$work/$2" ;;
    esac
    git -C "$work" add -A && git -C "$work" commit -q --allow-empty -m "$message" &&
        git -C "$work" push -q origin main || exit 1
    head=$(git -C "$work" rev-parse HEAD)
}
remote_ref() {
    git -C "$tmp/remote.git" rev-parse -q --verify "$1" 2> /dev/null || true
}
cat > "$work/scripts/ci-local.sh" <<'CHECK'
#!/usr/bin/env bash
printf 'strict=%s %s %s\n' "${RADIO_STRICT:-}" "$(pwd -P)" "$(git rev-parse HEAD)" >> "$DEFAULT_RAN"
CHECK
push_commit initial
base=$head
export DEFAULT_RAN="$tmp/default-ran"
cat > "$tmp/check.sh" <<'CHECK'
#!/usr/bin/env bash
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
printf '%s %s\n' "$(git rev-parse HEAD)" "$(pwd -P)" >> "$here/ran"
printf '%s\n' "$#" "$@" > "$here/arguments"
[ ! -f RED ] || exit 1
if [ -f "$here/flaky" ]; then
    rm "$here/flaky"
    exit 1
fi
CHECK
chmod 755 "$tmp/check.sh"
run() {
    local expected=$1
    shift
    (cd "$work" && bash "scripts/$1.sh" "${@:2}") > "$tmp/out" 2> "$tmp/err"
    status=$?
    [ "$status" -eq "$expected" ] || fail "exit $status, expected $expected: $(cat "$tmp/out" "$tmp/err")"
}
verify() {
    : > "$tmp/ran"
    run "$1" verify-main --remote origin -- "$tmp/check.sh"
}
last() {
    [ "$(tail -n 1 "$tmp/out")" = "$1" ] || fail "last line: $(tail -n 1 "$tmp/out"), expected $1"
}
err_last() {
    [ "$(tail -n 1 "$tmp/err")" = "$1" ] || fail "last stderr line: $(tail -n 1 "$tmp/err"), expected $1"
}
ref_is() {
    [ "$(remote_ref "$1")" = "$2" ] || fail "wrong $1"
}
runs() {
    [ "$(wc -l < "$tmp/ran" | tr -d ' ')" -eq "$1" ] || fail "expected $1 check runs"
}
clean_worktrees() {
    [ "$(git -C "$work" worktree list | wc -l)" -eq 1 ] || fail 'scratch worktree remains'
    [ -z "$(git -C "$work" rev-parse -q --verify refs/verify-main/main)" ] || fail 'private ref remains'
}
contains() {
    grep -F -q -- "$1" "$tmp/out" "$tmp/err" || fail "missing message: $1"
}
for args in '--bogus' '--remote' '--remote -x' '--remote nosuch' '--' '--remote=-x'; do
    # shellcheck disable=SC2086
    run 2 verify-main $args
    [ "$(cat "$tmp/err")" = 'usage: scripts/verify-main.sh [--remote <name>] [-- <check command>...]' ] || fail 'verify usage'
done
for args in '' 'main main'; do
    # shellcheck disable=SC2086
    run 2 release-check $args
    [ "$(cat "$tmp/err")" = 'usage: scripts/release-check.sh [--remote <name>] <commit>' ] || fail 'release usage'
done
run 2 release-check nosuchref
err_last 'release-check: nosuchref is not a commit'
run 2 verify-main --remote '--upload-pack=touch PWNED'
run 2 verify-main --remote ''
run 2 release-check --remote -x main
run 2 release-check --remote nosuch main

git init --bare -q -b main "$tmp/push.git"
git -C "$work" config remote.origin.pushurl "$tmp/push.git"
run 2 verify-main -- "$tmp/check.sh"
err_last 'usage: scripts/verify-main.sh [--remote <name>] [-- <check command>...]'
run 2 release-check main
err_last 'usage: scripts/release-check.sh [--remote <name>] <commit>'
git -C "$work" config --unset-all remote.origin.pushurl
git -C "$work" remote add no-main "$tmp/push.git"
run 1 verify-main --remote no-main -- "$tmp/check.sh"
err_last 'verify-main: no-main has no main'

git -C "$work" tag -a -m local local-only "$head"
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=push.followTags GIT_CONFIG_VALUE_0=true verify 0
ref_is refs/tags/local-only ''
last "green $head"
ref_is refs/green/main "$head"
runs 1
read -r checked directory < "$tmp/ran"
[ "$checked" = "$head" ] && [ "$directory" != "$work" ] || fail 'check used caller checkout'
clean_worktrees
run 0 verify-main --remote origin -- "$tmp/check.sh"
last "green is current: $head"
runs 1
push_commit batch-one
push_commit batch-two
verify 0
last "green $head"
runs 1
ref_is refs/green/main "$head"

# No default fetch mapping or stale tracking ref may choose the checked commit.
push_commit remote-main
local_only=$(printf 'local only\n' | git -C "$work" commit-tree "$head^{tree}" -p "$head")
git -C "$work" config --unset-all remote.origin.fetch
git -C "$work" update-ref refs/remotes/origin/main "$local_only"
verify 0
last "green $head"
ref_is refs/green/main "$head"
[ -z "$(git -C "$tmp/remote.git" for-each-ref --contains "$local_only" --format='%(refname)' 2> /dev/null)" ] || fail 'local-only commit reached remote ref'
if git -C "$tmp/remote.git" cat-file -e "$local_only" 2> /dev/null; then fail 'local-only object reached remote'; fi
[ -z "$(git -C "$work" rev-parse -q --verify refs/verify-main/main)" ] || fail 'private ref remains'
git -C "$tmp/remote.git" update-ref refs/green/main "$head"
git -C "$work" config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'

# The default check is strict and runs on remote main in the scratch worktree.
push_commit default-check
run 0 verify-main --remote origin
last "green $head"
read -r strict directory checked < "$tmp/default-ran"
[ "$strict" = strict=1 ] && [ "$directory" != "$work" ] && [ "$checked" = "$head" ] || fail 'wrong default check'

# The caller can be dirty and detached at an older commit.
push_commit newer
git -C "$work" checkout -q --detach "$base"
printf 'dirty\n' > "$work/uncommitted"
verify 0
read -r checked directory < "$tmp/ran"
[ "$checked" = "$head" ] && [ "$directory" != "$work" ] || fail 'wrong checkout checked'
[ -f "$work/uncommitted" ] || fail 'caller file changed'
rm "$work/uncommitted"
git -C "$work" checkout -q main
clean_worktrees

push_commit flaky
: > "$tmp/flaky"
verify 0
runs 2
contains "verify-main: ${head:0:12} passed on the rerun (the first run was red)"
ref_is refs/green/main "$head"
green=$head
push_commit red add RED
red=$head
verify 5
runs 2
last "red $head culprit $head"
[ "$(tail -n 2 "$tmp/out" | head -n 1)" = "verify-main: releases are frozen. Revert or fix ${head:0:12}, then run this again." ] || fail 'one culprit guidance'
ref_is refs/green/main "$green"
ref_is refs/green/frozen "$head"
clean_worktrees
run 1 release-check --remote origin "$green"
err_last "release-check: releases are frozen at ${red:0:12}: main went red twice there. Revert or fix it, and wait for the next green."
push_commit fix rm RED
verify 0
last "releases unfrozen (freeze at $red cleared)"
ref_is refs/green/main "$head"
ref_is refs/green/frozen ''
push_commit red-first add RED
first=$head
push_commit red-second
verify 6
last "red $head across 2 commits from $first to $head"
ref_is refs/green/frozen "$head"
contains "One of the 2 commits from ${first:0:12} to ${head:0:12} broke main"
push_commit fix-again rm RED
verify 0
ref_is refs/green/frozen ''
green=$head
run 0 release-check --remote origin main
last "release-check: ${head:0:12} passed the full check (green is ${head:0:12})."
git -C "$work" tag -a -m checked checked "$green"
run 0 release-check checked
push_commit unchecked
run 1 release-check main
err_last "release-check: ${head:0:12} has not passed the full check on main yet (green is ${green:0:12}). Release a green commit, or run scripts/verify-main.sh first."
git init --bare -q -b main "$tmp/empty.git"
git -C "$work" remote add empty "$tmp/empty.git"
run 1 release-check --remote empty main
err_last 'release-check: nothing on main has passed the full check yet.'
git -C "$work" push -q empty "$base:refs/green/main" || exit 1
run 1 release-check --remote empty main
err_last 'release-check: empty has no main.'
git -C "$work" remote add unreadable "$tmp/missing.git"
run 1 release-check --remote unreadable main
err_last 'release-check: could not read unreadable'
run 1 verify-main --remote unreadable -- "$tmp/check.sh"
err_last 'verify-main: could not read unreadable'
ref_is refs/heads/main "$head"

orphan=$(printf 'orphan\n' | git -C "$work" commit-tree "$head^{tree}")
git -C "$work" push -q origin "+$orphan:refs/green/main" || exit 1
verify 1
runs 0
err_last "verify-main: green ${orphan:0:12} is not on origin/main; nothing changed"
ref_is refs/green/main "$orphan"
run 1 release-check main
err_last "release-check: green ${orphan:0:12} is not on origin/main; move it back onto main by hand (git push origin +<a main commit>:refs/green/main)."
git -C "$tmp/remote.git" update-ref refs/green/main "$green"

# A pass must keep a freeze whose commit it does not contain.
git -C "$tmp/remote.git" update-ref refs/green/frozen "$orphan"
verify 0
last "green $head"
ref_is refs/green/frozen "$orphan"
contains "verify-main: releases stay frozen at ${orphan:0:12}, which is no longer on origin/main; remove the freeze with: git push origin :refs/green/frozen"
run 0 verify-main -- "$tmp/check.sh"
last "green is current: $head"
contains "verify-main: releases stay frozen at ${orphan:0:12}, which is no longer on origin/main; remove the freeze with: git push origin :refs/green/frozen"
git -C "$tmp/remote.git" update-ref -d refs/green/frozen
git -C "$tmp/remote.git" update-ref refs/green/main "$green"

# Competing writers: an up-to-date porcelain line must not count as our successful push.
cat > "$tmp/race.sh" <<'CHECK'
#!/usr/bin/env bash
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
git -C "$here/remote.git" update-ref refs/green/main "$(cat "$here/move-to")"
CHECK
printf '%s\n' "$head" > "$tmp/move-to"
git -C "$tmp/remote.git" update-ref refs/green/frozen "$orphan"
run 0 verify-main -- "$BASH" "$tmp/race.sh"
last "covered: green is $head"
contains "verify-main: releases stay frozen at ${orphan:0:12}, which is no longer on origin/main; remove the freeze with: git push origin :refs/green/frozen"
git -C "$tmp/remote.git" update-ref -d refs/green/frozen
push_commit lease-race
printf '%s\n' "$base" > "$tmp/move-to"
run 1 verify-main -- bash "$tmp/race.sh"
err_last 'verify-main: green moved during the check; nothing changed; run it again'
ref_is refs/green/main "$base"
clean_worktrees

cat > "$tmp/rewind.sh" <<'CHECK'
#!/usr/bin/env bash
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
git -C "$here/remote.git" update-ref refs/heads/main "$(git rev-parse HEAD^)"
CHECK
run 1 verify-main -- bash "$tmp/rewind.sh"
err_last "verify-main: origin/main no longer holds ${head:0:12}; nothing changed"
ref_is refs/green/main "$base"
clean_worktrees
git -C "$work" push -q origin main
printf 'exit 1\n' >> "$tmp/rewind.sh"
run 1 verify-main -- bash "$tmp/rewind.sh"
err_last "verify-main: origin/main no longer holds ${head:0:12}; nothing changed"
ref_is refs/green/frozen ''
ref_is refs/green/main "$base"
clean_worktrees
git -C "$work" push -q origin main

common=$(git -C "$work" rev-parse --git-common-dir)
case "$common" in /*) ;; *) common="$work/$common" ;; esac
sleep 30 &
sleeper=$!
ln -s "$sleeper" "$common/verify-main.lock"
: > "$tmp/ran"
run 0 verify-main -- "$tmp/check.sh"
last "verify-main: already running (pid $sleeper)"
runs 0
[ "$(readlink "$common/verify-main.lock")" = "$sleeper" ] || fail 'live lock removed'
kill "$sleeper"
wait "$sleeper" 2> /dev/null || true
rm "$common/verify-main.lock"
for target in "$sleeper" not-a-pid; do
    ln -s "$target" "$common/verify-main.lock"
    push_commit stale-lock
    verify 0
    runs 1
    [ ! -L "$common/verify-main.lock" ] || fail 'stale lock remains'
done
sleeper=''
push_commit strange-argument
# The literal shell syntax must reach the check as one argument.
# shellcheck disable=SC2016
run 0 verify-main -- "$tmp/check.sh" 'a b;$(touch PWNED)'
last "green $head"
# shellcheck disable=SC2016
[ "$(cat "$tmp/arguments")" = "$(printf '%s\n' 1 'a b;$(touch PWNED)')" ] || fail 'check argument was split or interpreted'
[ -z "$(find "$tmp" -name PWNED -print)" ] || fail 'check argument executed'
if [ "$(/bin/bash -c 'printf "%s" "$BASH_VERSINFO"')" = 3 ]; then
    push_commit stock-one
    push_commit stock-two
    (cd "$work" && /bin/bash scripts/verify-main.sh -- "$tmp/check.sh") > "$tmp/out" 2> "$tmp/err" || fail 'stock bash batch'
    last "green $head"
fi
ref_is refs/green/main "$head"
ref_is refs/green/frozen ''
[ "$(git -C "$tmp/remote.git" for-each-ref --format='%(refname)')" = "$(printf '%s\n' refs/green/main refs/heads/main)" ] || fail 'unexpected remote ref'
clean_worktrees
[ "$failed" -eq 0 ] || exit 1
printf 'verify_main_test: all passed\n'
