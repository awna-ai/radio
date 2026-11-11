#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

missing() {
    if [ "${RADIO_STRICT:-}" = 1 ]; then
        printf 'ci-local: %s is missing (RADIO_STRICT=1)\n' "$1" >&2
        exit 1
    fi
    printf 'ci-local: %s is missing, skipping %s\n' "$1" "$2"
}

if command -v shellcheck > /dev/null 2>&1; then
    echo 'shellcheck scripts/*.sh tests/*.sh'
    shellcheck scripts/*.sh tests/*.sh
else
    missing shellcheck 'the shell checks'
fi

echo 'bash tests/term_check_test.sh'
bash tests/term_check_test.sh
echo 'bash scripts/term-check.sh'
bash scripts/term-check.sh

if command -v gitleaks > /dev/null 2>&1; then
    if git rev-parse --verify -q HEAD > /dev/null; then
        echo 'gitleaks git --redact --no-banner .'
        gitleaks git --redact --no-banner .
    fi
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    mkdir "$tmp/tree"
    git ls-files -z --cached --others --exclude-standard > "$tmp/paths"
    while IFS= read -r -d '' path; do
        if [ -f "$path" ]; then
            printf '%s\0' "$path"
        fi
    done < "$tmp/paths" | tar --null -T - -cf - | tar -xf - -C "$tmp/tree"
    echo 'gitleaks dir --redact --no-banner <tmp>'
    gitleaks dir --redact --no-banner "$tmp/tree"
else
    missing gitleaks 'the secret scan'
fi

if command -v cargo > /dev/null 2>&1; then
    echo 'cargo fmt --all --check'
    cargo fmt --all --check
    echo 'cargo clippy --all-targets --locked -- -D warnings'
    cargo clippy --all-targets --locked -- -D warnings
    echo 'cargo test --locked'
    cargo test --locked
else
    missing cargo 'the Rust checks'
fi

echo 'ci-local: all passed'
