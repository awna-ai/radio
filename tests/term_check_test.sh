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
    case "$case_number" in
        5)
            output=$(printf '%s\n' "$output" | sed "s/^term-check: the path $term_lower.txt names a private term$/term-check: the path [redacted] names a private term/")
            ;;
        21)
            output=$(printf '%s\n' "$output" | sed 's/^term-check: the path lane_q.txt names a private term$/term-check: the path [redacted] names a private term/')
            ;;
        22)
            output=$(printf '%s\n' "$output" | sed 's/^term-check: the link docs points at a private term$/term-check: the link [redacted] points at a private term/')
            ;;
    esac
    if printf '%s\n' "$output" | grep -F -i -q -e "$term_lower" -e "$phrase"; then
        printf 'term_check_test: FAIL %s: output exposed a private term\n' "$name"
        failed=1
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

new_case 17
printf '%s\n' "$term_title" > notes.txt
commit_file notes.txt
printf 'hello\n' > notes.txt
commit_file notes.txt
expect removed-content 1 'the history holds a file naming a private term'

new_case 18
printf 'hello\n' > "$term_lower.txt"
commit_file "$term_lower.txt"
git mv "$term_lower.txt" clean.txt
git commit -qm 'rename text'
expect renamed-path 1 'the history holds a path naming a private term'

new_case 19
printf 'let %s_client = 1;\n' "$term_lower" > code.rs
commit_file code.rs
expect underscore-identifier 1 'code.rs:1 names a private term'

new_case 20
printf 'struct %sClient;\n' "$term_title" > code.rs
commit_file code.rs
expect camel-identifier 1 'code.rs:1 names a private term'

new_case 21
printf 'hello\n' > lane_q.txt
commit_file lane_q.txt
expect identifier-path 1 'the path'

new_case 22
ln -s "$term_lower-notes" docs
commit_file docs
expect dangling-link 1 'the link docs points at a private term'

new_case 23
printf '%s\n' "$term_title" > "$tmp/target.txt"
ln -s "$tmp/target.txt" outside
commit_file outside
expect outside-link 0 'term-check: clean'

new_case 24
printf 'a\0%s\0' "$term_title" > blob.bin
commit_file blob.bin
expect binary 1 'blob.bin (binary) names a private term'

new_case 25
printf 'Rename %sClient\n' "$term_title" > "$tmp/message.txt"
expect message-identifier 1 'the message names a private term' --message "$tmp/message.txt"

new_case 26
printf 'invoice_total\nNot%ss\n' "$term_title" > words.txt
commit_file words.txt
expect identifier-whole-words 0 'term-check: clean'

new_case 27
printf 'a\0%s\0' "$term_title" > blob.bin
commit_file blob.bin
git rm -q blob.bin
git commit -qm 'remove binary'
expect removed-binary 1 'the history holds a file naming a private term'

new_case 28
printf 'notes.txt binary\n' > .gitattributes
printf '%s\n' "$term_title" > notes.txt
git add .gitattributes notes.txt
git commit -qm 'add notes'
git rm -q .gitattributes notes.txt
git commit -qm 'remove notes'
expect attributed-binary 1 'the history holds a file naming a private term'

new_case 29
hello
git checkout -q -b side
printf '%s\n' "$term_title" > notes.txt
commit_file notes.txt
git checkout -q -
expect side-branch 1 'the history holds a file naming a private term'

new_case 30
hello
git tag -a v1 -m "$term_title release"
expect tag-message 1 'tag '
expect tag-message-diagnostic 1 'names a private term in its message'

new_case 31
name=$(printf 'a\n%s.txt' "$term_lower")
: > "$name"
commit_file "$name"
git mv "$name" clean.txt
git commit -qm 'rename text'
expect newline-path 1 'the history holds a path naming a private term'

new_case 32
printf 'let HTTP%s = 1;\n' "$term_title" > code.rs
commit_file code.rs
expect acronym-identifier 1 'code.rs:1 names a private term'

new_case 33
printf 'let %s2_client = 1;\n' "$term_lower" > code.rs
commit_file code.rs
expect digit-identifier 1 'code.rs:1 names a private term'

new_case 34
hello
expect missing-message 2 "can't read the message file" --message "$tmp/absent.txt"

new_case 35
printf 'clean\n' > notes.txt
commit_file notes.txt
sha=$(git rev-parse HEAD:notes.txt)
rm ".git/objects/${sha:0:2}/${sha:2}"
expect missing-blob 2 'git could not'

new_case 36
printf 'hello\n' > hello.txt
git add hello.txt
git -c user.name="$term_title" commit -qm 'add text'
expect author-identity 0 'term-check: clean'

new_case 37
hello
blob=$(printf 'x\n' | git hash-object -w --stdin)
tree=$(printf '100644 blob %s\t%s.txt\n' "$blob" "$term_lower" | git mktree)
git tag -a t1 -m 'a tree' "$tree"
expect tagged-tree-path 1 'the history holds a path naming a private term'

new_case 38
printf 'x\377\376\nmy%s\n' "$term_title" > odd.txt
commit_file odd.txt
export LC_ALL=en_US.UTF-8
expect invalid-utf8-content 1 'odd.txt:2 names a private term'
unset LC_ALL

new_case 39
printf 'a\0%s\0' "$term_title" > blob.bin
commit_file blob.bin
git rm -q blob.bin
git commit -qm 'remove binary'
export LC_ALL=en_US.UTF-8
expect utf8-removed-binary 1 'the history holds a file naming a private term'
unset LC_ALL

new_case 40
printf '%s\n' "$term_title" > notes.txt
commit_file notes.txt
original=$(git rev-parse HEAD)
git rm -q notes.txt
git commit -qm 'remove notes'
replacement=$(git commit-tree "$(git rev-parse 'HEAD^{tree}')" -m clean)
git replace "$original" "$replacement"
expect replaced-history 1 'the history holds a file naming a private term'

new_case 41
hello
: > "$(git rev-parse --git-path info/grafts)"
expect grafts-file 2 'grafts file'

new_case 42
printf 'clean\n' > notes.txt
commit_file notes.txt
sha=$(git rev-parse HEAD:notes.txt)
chmod u+w ".git/objects/${sha:0:2}/${sha:2}"
printf 'garbage' > ".git/objects/${sha:0:2}/${sha:2}"
expect corrupt-blob 2 'git could not'

new_case 43
hello
git tag -a inner -m "$term_title inner"
git tag -a outer -m clean inner
git tag -d inner > /dev/null
expect nested-tag-message 1 'names a private term in its message'

new_case 44
hello
git branch "$term_lower"
expect ref-name 1 'a ref name names a private term'

if [ "$failed" -ne 0 ]; then
    exit 1
fi
printf 'term_check_test: all passed\n'
