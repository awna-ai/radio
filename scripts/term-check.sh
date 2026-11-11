#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
if [ "$#" -ne 0 ] && { [ "$#" -ne 2 ] || [ "$1" != --message ]; }; then
    echo 'usage: term-check.sh [--message <file>]' >&2
    exit 2
fi

terms=${RADIO_TERMS_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/radio-dev/terms.txt}
if [ ! -f "$terms" ] || [ ! -r "$terms" ]; then
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'term-check: no term list at %s\n' "$terms" >&2
        exit 1
    fi
    printf 'term-check: no term list at %s, skipped\n' "$terms"
    exit 0
fi

list=$(mktemp)
paths=''
trap 'rm -f "$list" "$paths"' EXIT
sed -e 's/\r$//' -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$terms" > "$list"
if [ ! -s "$list" ]; then
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'term-check: the term list at %s is empty\n' "$terms" >&2
        exit 1
    fi
    printf 'term-check: the term list at %s is empty, skipped\n' "$terms"
    exit 0
fi

if [ "$#" -eq 2 ]; then
    if grep -F -i -w -f "$list" -- "$2" > /dev/null; then
        echo 'term-check: the message names a private term'
        exit 1
    else
        status=$?
        if [ "$status" -ne 1 ]; then
            exit "$status"
        fi
    fi
    echo 'term-check: the message is clean'
    exit 0
fi

paths=$(mktemp)
git -C "$root" ls-files -z --cached --others --exclude-standard > "$paths"
files=0
commits=0
hit=0
while IFS= read -r -d '' path; do
    if [ ! -f "$root/$path" ]; then
        continue
    fi
    files=$((files + 1))
    if lines=$(grep -F -i -w -I -n -f "$list" -- "$root/$path" | cut -d: -f1); then
        while IFS= read -r line; do
            printf 'term-check: %s:%s names a private term\n' "$path" "$line"
        done <<< "$lines"
        hit=1
    else
        status=$?
        if [ "$status" -ne 1 ]; then
            exit "$status"
        fi
    fi
    if printf '%s\n' "$path" | grep -F -i -w -f "$list" > /dev/null; then
        printf 'term-check: the path %s names a private term\n' "$path"
        hit=1
    fi
done < "$paths"

if git -C "$root" rev-parse --verify -q HEAD > /dev/null; then
    history=$(git -C "$root" rev-list HEAD)
    while IFS= read -r sha; do
        commits=$((commits + 1))
        message=$(git -C "$root" log -1 --format=%B "$sha")
        if printf '%s\n' "$message" | grep -F -i -w -f "$list" > /dev/null; then
            printf 'term-check: commit %s names a private term in its message\n' "${sha:0:7}"
            hit=1
        fi
    done <<< "$history"
fi

if [ "$hit" -ne 0 ]; then
    exit 1
fi
printf 'term-check: clean (%s files, %s commits)\n' "$files" "$commits"
