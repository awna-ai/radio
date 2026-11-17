#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
    echo 'usage: scripts/ci-local.sh [--tier full|touched] [--list]' >&2
    exit 2
}
tier=full
listing=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --tier)
            [ "$#" -ge 2 ] || usage
            case "$2" in full | touched) tier=$2 ;; *) usage ;; esac
            shift 2
            ;;
        --list)
            listing=1
            shift
            ;;
        *) usage ;;
    esac
done

skipped=0
missing() {
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'ci-local: %s is missing (RADIO_STRICT=1)\n' "$1" >&2
        exit 1
    fi
    printf 'ci-local: %s is missing, skipping %s\n' "$1" "$2"
    skipped=$((skipped + 1))
}
# Print a step, then run it unless --list asked only for the steps.
run() {
    printf '%s\n' "$*"
    if [ "$listing" -eq 0 ]; then
        "$@"
    fi
}
# The package a crate folder holds; empty when the folder no longer holds one.
package_in() {
    sed -n 's/^name = "\(.*\)"$/\1/p' "crates/$1/Cargo.toml" 2> /dev/null | head -n 1 || true
}

# What the run checks. The full tier checks everything; the touched tier only what the change since
# the base (RADIO_CI_BASE, default origin/main) can break. The repo-wide scans run in both.
all_shell_tests=(term_check_test release_test ci_local_test)
shell_tests=("${all_shell_tests[@]}")
crates=()
fuzz=1
if [ "$tier" = touched ]; then
    base=${RADIO_CI_BASE:-origin/main}
    everything=0
    touched_crates=()
    touched_shell=()
    fork=''
    case "$base" in -*) ;; *) fork=$(git merge-base "$base" HEAD 2> /dev/null) || fork='' ;; esac
    # A base at or past HEAD hides the committed changes from the list; the default is origin/main.
    if [ -n "$fork" ] && changed=$({
        git diff --name-only --no-renames "$fork" &&
            git ls-files --others --exclude-standard
    } | sort -u); then
        while IFS= read -r path; do
            [ -n "$path" ] || continue
            case "$path" in
                crates/*/*)
                    folder=${path#crates/}
                    touched_crates+=("$(package_in "${folder%%/*}")")
                    ;;
                # Files the crates' own tests read.
                contract/* | CONTRACT.md) touched_crates+=(radio-client) ;;
                README.md) touched_crates+=(radio) ;;
                fuzz/*) touched_crates+=(radio-link) ;;
                scripts/term-check.sh | tests/term_check_test.sh) touched_shell+=(term_check_test) ;;
                scripts/release.sh | tests/release_test.sh) touched_shell+=(release_test) ;;
                scripts/ci-local.sh | tests/ci_local_test.sh) touched_shell+=(ci_local_test) ;;
                *.md | LICENSE | .gitignore) ;;
                *) everything=1 ;;
            esac
        done <<< "$changed"
    else
        printf 'ci-local: the base %s does not resolve, or the changes since it cannot be read; checking everything\n' \
            "$base" >&2
        everything=1
    fi
    for crate in "${touched_crates[@]+"${touched_crates[@]}"}"; do
        [ -n "$crate" ] || everything=1
    done
    if [ "$everything" -eq 0 ] && [ "${#touched_crates[@]}" -gt 0 ]; then
        if command -v cargo > /dev/null 2>&1; then
            # The changed crates and every workspace crate that depends on them, dev-dependencies included.
            here=$(pwd -P)
            tree=''
            for crate in $(printf '%s\n' "${touched_crates[@]}" | sort -u); do
                found=$(cargo tree --locked --workspace -i "$crate" -e normal,build,dev --prefix none --format '{p}') \
                    || everything=1
                tree+="$found"$'\n'
            done
            while IFS= read -r crate; do
                crates+=("$crate")
            done < <(printf '%s' "$tree" | awk -v folder="($here/crates/" 'index($0, folder) { print $1 }' | sort -u)
            # A query that finds not even the changed crate itself is not to be trusted.
            [ "${#crates[@]}" -gt 0 ] || everything=1
        else
            everything=1
        fi
    fi
    if [ "$everything" -eq 1 ]; then
        tier=full
        crates=()
    else
        shell_tests=()
        for test in "${all_shell_tests[@]}"; do
            if [[ " ${touched_shell[*]-} " == *" $test "* ]]; then
                shell_tests+=("$test")
            fi
        done
        # The fuzz targets depend on radio-link.
        fuzz=0
        for crate in "${crates[@]+"${crates[@]}"}"; do
            [ "$crate" != radio-link ] || fuzz=1
        done
    fi
fi

if command -v shellcheck > /dev/null 2>&1; then
    echo 'shellcheck scripts/*.sh tests/*.sh'
    if [ "$listing" -eq 0 ]; then
        shellcheck scripts/*.sh tests/*.sh
    fi
else
    missing shellcheck 'the shell checks'
fi

for test in "${shell_tests[@]+"${shell_tests[@]}"}"; do
    run bash "tests/$test.sh"
done
echo 'bash scripts/term-check.sh'
if [ "$listing" -eq 0 ]; then
    term_status=0
    term_output=$(bash scripts/term-check.sh) || term_status=$?
    printf '%s\n' "$term_output"
    if [ "$term_status" -ne 0 ]; then
        exit "$term_status"
    fi
    if [[ "$term_output" == *", skipped" ]]; then
        skipped=$((skipped + 1))
    fi
fi

if command -v gitleaks > /dev/null 2>&1; then
    if git rev-parse --verify -q HEAD > /dev/null; then
        run gitleaks git --redact --no-banner .
        echo "{ git log --all --format=%B; git for-each-ref refs/tags --format='%(contents)'; } | gitleaks stdin --redact --no-banner"
        if [ "$listing" -eq 0 ]; then
            { git log --all --format=%B; git for-each-ref refs/tags --format='%(contents)'; } | gitleaks stdin --redact --no-banner
        fi
    fi
    echo 'gitleaks dir --redact --no-banner <tmp>'
    if [ "$listing" -eq 0 ]; then
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        mkdir "$tmp/tree"
        git ls-files -z --cached --others --exclude-standard > "$tmp/paths"
        while IFS= read -r -d '' path; do
            if [ -f "$path" ]; then
                printf '%s\0' "$path"
            fi
        done < "$tmp/paths" | tar --null -T - -cf - | tar -xf - -C "$tmp/tree"
        gitleaks dir --redact --no-banner "$tmp/tree"
    fi
else
    missing gitleaks 'the secret scan'
fi

if command -v cargo > /dev/null 2>&1; then
    run cargo fmt --all --check
    run cargo fmt --manifest-path fuzz/Cargo.toml --check
    # radio-panels turns on serde_json features the client and the rooms store ship without; each tier
    # tests them again on their own, as they ship. The daemon depends on radio-panels, so it ships with them.
    if [ "$tier" = full ]; then
        run cargo clippy --all-targets --locked -- -D warnings
        run cargo test --locked
        run cargo test --locked -p radio-client -p radio-rooms
    elif [ "${#crates[@]}" -gt 0 ]; then
        packages=()
        alone=()
        for crate in "${crates[@]}"; do
            packages+=(-p "$crate")
            case "$crate" in radio-client | radio-rooms) alone+=(-p "$crate") ;; esac
        done
        run cargo clippy --all-targets --locked "${packages[@]}" -- -D warnings
        run cargo test --locked "${packages[@]}"
        # radio-link's one_builder test reads every crate's sources, so it runs for any crate's change.
        if [ "$fuzz" -eq 0 ]; then
            run cargo test --locked -p radio-link --test one_builder
        fi
        if [ "${#alone[@]}" -gt 0 ] && [ "${#alone[@]}" -lt "${#packages[@]}" ]; then
            run cargo test --locked "${alone[@]}"
        fi
    fi
    if [ "$fuzz" -eq 1 ]; then
        run cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
        run cargo test --manifest-path fuzz/Cargo.toml --locked
    fi
else
    missing cargo 'the Rust checks'
fi

if [ "$listing" -eq 1 ]; then
    exit 0
fi
if [ "$skipped" -eq 0 ]; then
    echo 'ci-local: all passed'
else
    printf 'ci-local: all passed, %s skipped\n' "$skipped"
fi
