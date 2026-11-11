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
scan=$(mktemp)
tree=''
trap 'rm -f "$list" "$paths" "$scan" "$tree"' EXIT
sed -e 's/\r$//' -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$terms" > "$list"
if [ ! -s "$list" ]; then
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'term-check: the term list at %s is empty\n' "$terms" >&2
        exit 1
    fi
    printf 'term-check: the term list at %s is empty, skipped\n' "$terms"
    exit 0
fi

# Read the entire input before matching so neither pass loses data to a pipe.
normalize() {
    sed -E 's/_/ /g; s/([a-z0-9])([A-Z])/\1 \2/g' "$@"
}

matches() {
    cat > "$scan"
    grep -F -i -w -f "$list" -- "$scan" > /dev/null ||
        normalize "$scan" | grep -F -i -w -f "$list" > /dev/null
}

if [ "$#" -eq 2 ]; then
    if matches < "$2"; then
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
    if [ ! -L "$root/$path" ] && [ ! -f "$root/$path" ]; then
        continue
    fi
    files=$((files + 1))
    if [ -L "$root/$path" ]; then
        if readlink "$root/$path" | matches; then
            printf 'term-check: the link %s points at a private term\n' "$path"
            hit=1
        fi
    elif grep -I -e '' -- "$root/$path" > /dev/null; then
        lines=$({
            grep -F -i -w -I -n -f "$list" -- "$root/$path" || [ "$?" -eq 1 ]
            normalize "$root/$path" | grep -F -i -w -n -f "$list" || [ "$?" -eq 1 ]
        } | cut -d: -f1 | sort -n -u)
        if [ -n "$lines" ]; then
            while IFS= read -r line; do
                printf 'term-check: %s:%s names a private term\n' "$path" "$line"
            done <<< "$lines"
            hit=1
        fi
    elif grep -a -F -i -w -f "$list" -- "$root/$path" > /dev/null; then
        printf 'term-check: %s (binary) names a private term\n' "$path"
        hit=1
    fi
    if printf '%s\n' "$path" | matches; then
        printf 'term-check: the path %s names a private term\n' "$path"
        hit=1
    fi
done < "$paths"

if git -C "$root" rev-parse --verify -q HEAD > /dev/null; then
    history=$(git -C "$root" rev-list HEAD)
    while IFS= read -r sha; do
        commits=$((commits + 1))
        message=$(git -C "$root" log -1 --format=%B "$sha")
        if printf '%s\n' "$message" | matches; then
            printf 'term-check: commit %s names a private term in its message\n' "${sha:0:7}"
            hit=1
        fi
    done <<< "$history"
    tree=$(mktemp)
    while IFS= read -r sha; do
        commit_hit=0
        if git -C "$root" ls-tree -r --name-only "$sha" | matches; then
            commit_hit=1
        fi
        if { git -C "$root" grep -I -h --no-color -e '' "$sha" -- || [ "$?" -eq 1 ]; } | matches; then
            commit_hit=1
        fi
        git -C "$root" ls-tree -r -z "$sha" > "$tree"
        while IFS= read -r -d '' entry; do
            metadata=${entry%%$'\t'*}
            read -r mode _kind blob <<< "$metadata"
            if [ "$mode" = 120000 ]; then
                if git -C "$root" cat-file -p "$blob" | matches; then
                    commit_hit=1
                fi
            fi
        done < "$tree"
        if [ "$commit_hit" -ne 0 ]; then
            printf 'term-check: commit %s holds a file or path naming a private term\n' "${sha:0:7}"
            hit=1
        fi
    done <<< "$history"
fi

if [ "$hit" -ne 0 ]; then
    exit 1
fi
printf 'term-check: clean (%s files, %s commits)\n' "$files" "$commits"
