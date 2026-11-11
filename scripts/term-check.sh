#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
export GIT_NO_REPLACE_OBJECTS=1

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
normalized=$(mktemp)
objects=''
history=''
trap 'rm -f "$list" "$paths" "$scan" "$objects" "$history" "$normalized"' EXIT
sed -e 's/\r$//' -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$terms" > "$list"
if [ ! -s "$list" ]; then
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'term-check: the term list at %s is empty\n' "$terms" >&2
        exit 1
    fi
    printf 'term-check: the term list at %s is empty, skipped\n' "$terms"
    exit 0
fi

normalize() {
    sed -E 's/_/ /g; s/([a-z0-9])([A-Z])/\1 \2/g; s/([A-Z])([A-Z][a-z])/\1 \2/g; s/([A-Za-z])([0-9])/\1 \2/g; s/([0-9])([A-Za-z])/\1 \2/g' "$@"
}

# hits <file>: 0 on a hit, 1 on none; any grep error exits 2.
hits() {
    local status
    if grep -a -F -i -w -f "$list" -- "$1" > /dev/null; then
        return 0
    else
        status=$?
        [ "$status" -eq 1 ] || exit 2
    fi
    normalize "$1" > "$normalized" || exit 2
    if grep -a -F -i -w -f "$list" -- "$normalized" > /dev/null; then
        return 0
    else
        status=$?
        [ "$status" -eq 1 ] || exit 2
    fi
    return 1
}

if [ "$#" -eq 2 ]; then
    if [ ! -f "$2" ] || [ ! -r "$2" ]; then
        echo "term-check: can't read the message file" >&2
        exit 2
    fi
    if hits "$2"; then
        echo 'term-check: the message names a private term'
        exit 1
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
        readlink "$root/$path" > "$scan" || exit 2
        if hits "$scan"; then
            printf 'term-check: the link %s points at a private term\n' "$path"
            hit=1
        fi
    else
        text_status=0
        grep -I -e '' -- "$root/$path" > /dev/null || text_status=$?
        if [ "$text_status" -gt 1 ]; then
            exit 2
        fi
        if hits "$root/$path"; then
            if [ "$text_status" -eq 0 ]; then
                lines=$({
                    grep -a -F -i -w -I -n -f "$list" -- "$root/$path" || { [ "$?" -eq 1 ] || exit 2; }
                    normalize "$root/$path" > "$normalized" || exit 2
                    grep -a -F -i -w -n -f "$list" -- "$normalized" || { [ "$?" -eq 1 ] || exit 2; }
                } | cut -d: -f1 | sort -n -u) || exit 2
                while IFS= read -r line; do
                    printf 'term-check: %s:%s names a private term\n' "$path" "$line"
                done <<< "$lines"
            else
                printf 'term-check: %s (binary) names a private term\n' "$path"
            fi
            hit=1
        fi
    fi
    printf '%s\n' "$path" > "$scan"
    if hits "$scan"; then
        printf 'term-check: the path %s names a private term\n' "$path"
        hit=1
    fi
done < "$paths"

grafts=$(git -C "$root" rev-parse --git-path info/grafts) || exit 2
case "$grafts" in
    /*) ;;
    *) grafts="$root/$grafts" ;;
esac
if [ -f "$grafts" ]; then
    echo 'term-check: a grafts file rewrites the history; remove it first' >&2
    exit 2
fi
git -C "$root" for-each-ref --format='%(refname)' > "$scan" || exit 2
if hits "$scan"; then
    echo 'term-check: a ref name names a private term'
    hit=1
fi

objects=$(mktemp)
if ! git -C "$root" rev-list --all --objects --no-object-names \
    | git -C "$root" cat-file --batch-check='%(objecttype) %(objectname)' > "$objects"; then
    echo 'term-check: git could not list the history' >&2
    exit 2
fi
if ! awk '$1 != "commit" && $1 != "tree" && $1 != "blob" && $1 != "tag" { exit 1 }' "$objects"; then
    echo 'term-check: git could not read the history' >&2
    exit 2
fi
history=$(mktemp)
if ! awk '$1 != "tree" { print $2 }' "$objects" \
    | git -C "$root" cat-file --batch='' > "$scan"; then
    echo 'term-check: git could not read the history' >&2
    exit 2
fi
grep -a -v -E '^(author|committer|tagger) .* <[^>]*> [0-9]+ [+-][0-9]{4}$' "$scan" > "$history" \
    || { [ "$?" -eq 1 ] || exit 2; }
if ! git -C "$root" log --all -m --no-renames --name-only -z --format= >> "$history"; then
    echo 'term-check: git could not read the history' >&2
    exit 2
fi
if ! git -C "$root" rev-list --all --objects >> "$history"; then
    echo 'term-check: git could not read the history' >&2
    exit 2
fi
tr '\0' '\n' < "$history" > "$scan" || exit 2
if hits "$scan"; then
    history_hit=$hit
    hit=0
    while read -r kind sha; do
        case "$kind" in
            commit|tag)
                if ! git -C "$root" cat-file "$kind" "$sha" | sed '1,/^$/d' > "$scan"; then
                    printf 'term-check: git could not read %s %s\n' "$kind" "${sha:0:7}" >&2
                    exit 2
                fi
                if [ "$kind" = commit ]; then
                    commits=$((commits + 1))
                fi
                diagnostic="$kind ${sha:0:7} names a private term in its message"
                ;;
            tree)
                if ! git -C "$root" ls-tree -z --name-only "$sha" | tr '\0' '\n' > "$scan"; then
                    printf 'term-check: git could not read %s %s\n' "$kind" "${sha:0:7}" >&2
                    exit 2
                fi
                diagnostic="the history holds a path naming a private term (tree ${sha:0:7})"
                ;;
            blob)
                if ! git -C "$root" cat-file blob "$sha" > "$scan"; then
                    printf 'term-check: git could not read %s %s\n' "$kind" "${sha:0:7}" >&2
                    exit 2
                fi
                diagnostic="the history holds a file naming a private term (blob ${sha:0:7})"
                ;;
            *)
                echo 'term-check: git could not list the history' >&2
                exit 2
                ;;
        esac
        if hits "$scan"; then
            printf 'term-check: %s\n' "$diagnostic"
            hit=1
        fi
    done < "$objects"
    if [ "$hit" -eq 0 ]; then
        echo 'term-check: the history names a private term'
        hit=1
    fi
    if [ "$history_hit" -ne 0 ]; then
        hit=1
    fi
else
    commits=$(awk '$1 == "commit"' "$objects" | wc -l | tr -d '[:space:]')
fi

if [ "$hit" -ne 0 ]; then
    exit 1
fi
printf 'term-check: clean (%s files, %s commits)\n' "$files" "$commits"
