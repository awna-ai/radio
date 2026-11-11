#!/usr/bin/env bash
set -u -o pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unset RADIO_STRICT
terms="$tmp/terms.txt"
term_title=Bramble'wick'
term_lower=bramble'wick'
term_upper=BRAMBLE'WICK'
phrase='lane ''Q'
printf '# invented terms\n\n%s\n%s\n' "$term_title" "$phrase" > "$terms"
failed=0

new_case() {
    case_number=$1
    repo="$tmp/case-$case_number"
    mkdir -p "$repo/scripts"
    git init -q "$repo"
    git -C "$repo" config user.name Test
    git -C "$repo" config user.email test@example.invalid
    git -C "$repo" config commit.gpgsign false
    # The copied checker is fixture tooling, not a file under test.
    printf 'scripts/\n' > "$repo/.git/info/exclude"
    if [ -f "$root/scripts/term-check.sh" ]; then
        cp "$root/scripts/term-check.sh" "$repo/scripts/term-check.sh"
    fi
    cd "$repo" || exit 1
    export RADIO_TERMS_FILE="$terms"
    unset RADIO_STRICT
}

commit_file() {
    git add -- "$1" && git commit -qm 'add text'
}

hello() {
    printf 'hello\n' > hello.txt
    commit_file hello.txt
}

expect() {
    local name=$1 expected=$2 wanted=$3 output actual
    shift 3
    output=$(bash scripts/term-check.sh "$@" 2>&1)
    actual=$?
    if [ "$actual" -ne "$expected" ]; then
        printf 'term_check_test: FAIL %s: expected exit %s, got %s\n' "$name" "$expected" "$actual"
        failed=1
    fi
    if ! printf '%s\n' "$output" | grep -F -q -- "$wanted"; then
        printf 'term_check_test: FAIL %s: expected diagnostic absent\n' "$name"
        failed=1
    fi
    if [ "$case_number" -ne 5 ] && printf '%s\n' "$output" | grep -F -i -q -e "$term_lower" -e "$phrase"; then
        printf 'term_check_test: FAIL %s: output exposed a private term\n' "$name"
        failed=1
    fi
    if [ "$case_number" -eq 5 ]; then
        output=$(printf '%s\n' "$output" | sed "s/the path $term_lower.txt names a private term/the path [redacted] names a private term/g")
        if printf '%s\n' "$output" | grep -F -i -q -e "$term_lower" -e "$phrase"; then
            printf 'term_check_test: FAIL %s: output exposed a private term beyond the path\n' "$name"
            failed=1
        fi
    fi
}

new_case 1
hello
expect committed 0 'term-check: clean (1 files, 1 commits)'

new_case 2
printf 'hello\nthe %s app\n' "$term_upper" > notes.txt
commit_file notes.txt
expect content 1 'term-check: notes.txt:2 names a private term'

new_case 3
hello
printf '%s\n' "$term_title" > draft.md
expect untracked 1 'term-check: draft.md:1 names a private term'

new_case 4
hello
printf 'skip.txt\n' > .gitignore
commit_file .gitignore
printf '%s\n' "$term_title" > skip.txt
expect ignored 0 'term-check: clean'

new_case 5
printf 'hello\n' > "$term_lower.txt"
commit_file "$term_lower.txt"
expect path 1 "term-check: the path $term_lower.txt names a private term"

new_case 6
printf 'hello\n' > hello.txt
git add hello.txt
git commit -qm "add $phrase notes"
sha=$(git rev-parse --short=7 HEAD)
expect history 1 "term-check: commit $sha names a private term in its message"

new_case 7
printf '%ss\nplane Quiet\nlane Quiet\n' "$term_title" > words.txt
commit_file words.txt
expect whole-words 0 'term-check: clean'

new_case 8
hello
export RADIO_TERMS_FILE="$tmp/missing.txt"
expect missing-list 0 "term-check: no term list at $RADIO_TERMS_FILE, skipped"

new_case 9
hello
export RADIO_TERMS_FILE="$tmp/missing.txt" RADIO_STRICT=1
expect strict-missing-list 1 "term-check: no term list at $RADIO_TERMS_FILE"

new_case 10
hello
printf '# invented terms\n\n  # comment\n' > "$tmp/empty.txt"
export RADIO_TERMS_FILE="$tmp/empty.txt" RADIO_STRICT=1
expect strict-empty-list 1 "term-check: the term list at $RADIO_TERMS_FILE is empty"

new_case 11
hello
export RADIO_TERMS_FILE="$tmp/empty.txt"
expect empty-list 0 "term-check: the term list at $RADIO_TERMS_FILE is empty, skipped"

new_case 12
hello
printf 'Fix Lane q wording\n' > "$tmp/message.txt"
expect message-hit 1 'term-check: the message names a private term' --message "$tmp/message.txt"

new_case 13
hello
printf 'Fix the wording\n' > "$tmp/message.txt"
expect message-clean 0 'term-check: the message is clean' --message "$tmp/message.txt"

new_case 14
printf 'hello\n' > gone.txt
commit_file gone.txt
rm gone.txt
expect deleted 0 'term-check: clean'

new_case 15
printf 'hello\n' > hello.txt
expect unborn 0 'term-check: clean (1 files, 0 commits)'

new_case 16
printf '%s\r\n' "$term_title" > "$tmp/windows.txt"
export RADIO_TERMS_FILE="$tmp/windows.txt"
printf '%s\n' "$term_lower" > words.txt
commit_file words.txt
expect line-endings 1 'names a private term'

if [ "$failed" -ne 0 ]; then
    exit 1
fi
printf 'term_check_test: all passed\n'
