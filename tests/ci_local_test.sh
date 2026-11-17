#!/usr/bin/env bash
set -u -o pipefail

# Checks which steps ci-local.sh's tiers choose, from --list in a throwaway copy of the workspace.
root=$(cd "$(dirname "$0")/.." && pwd -P)
tmp=$(mktemp -d)
tmp=$(cd "$tmp" && pwd -P)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/calls"
export STUB_ROOT="$tmp"
STUB_CARGO=$(command -v cargo)
export STUB_CARGO
export PATH="$tmp/bin:$PATH"
failed=0

# --list runs nothing: every tool but cargo's dependency queries records its call and fails.
cat > "$tmp/bin/cargo" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = tree ]; then
    case "${STUB_TREE:-}" in
        fail) exit 101 ;;
        empty) exit 0 ;;
    esac
fi
case "$1" in
    tree | metadata) exec "$STUB_CARGO" "$@" ;;
esac
printf 'cargo %s\n' "$*" >> "$STUB_ROOT/calls/ran"
exit 1
STUB
for tool in shellcheck gitleaks; do
    cat > "$tmp/bin/$tool" <<'STUB'
#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "$STUB_ROOT/calls/ran"
exit 1
STUB
done
chmod 755 "$tmp/bin/"*

repo="$tmp/repo"
mkdir "$repo"
git -C "$root" archive HEAD Cargo.toml Cargo.lock crates fuzz README.md CONTRACT.md contract scripts tests \
    | tar -xf - -C "$repo"
cp "$root/scripts/ci-local.sh" "$repo/scripts/ci-local.sh"
git -C "$repo" init -q
git -C "$repo" add -A
git -C "$repo" -c user.name=t -c user.email=t@example.invalid commit -q -m fixture

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failed=1
}
reset() {
    git -C "$repo" reset -q --hard
    git -C "$repo" clean -qfd
}
list() {
    (cd "$repo" && RADIO_CI_BASE=HEAD bash scripts/ci-local.sh "$@" > "$tmp/out" 2> "$tmp/err")
}
has() {
    grep -F -x -q -- "$2" "$tmp/out" || fail "$1: no line: $2"
}
lacks() {
    if grep -F -q -- "$2" "$tmp/out"; then
        fail "$1: unexpected: $2"
    fi
}
always() {
    has "$1" 'shellcheck scripts/*.sh tests/*.sh'
    has "$1" 'bash scripts/term-check.sh'
    has "$1" 'gitleaks git --redact --no-banner .'
    has "$1" 'gitleaks dir --redact --no-banner <tmp>'
    has "$1" 'cargo fmt --all --check'
    has "$1" 'cargo fmt --manifest-path fuzz/Cargo.toml --check'
}
fuzz_test='cargo test --manifest-path fuzz/Cargo.toml --locked'

# A change in one crate checks it and the crates that depend on it, and nothing else.
reset
printf '// touched\n' >> "$repo/crates/radio-rooms/src/lib.rs"
list --tier touched --list || fail "rooms: exit $?"
always rooms
has rooms 'cargo clippy --all-targets --locked -p radio -p radio-rooms -- -D warnings'
has rooms 'cargo test --locked -p radio -p radio-rooms'
has rooms 'cargo test --locked -p radio-rooms'
lacks rooms radio-client
lacks rooms 'cargo clippy --all-targets --locked -p radio -p radio-link'
# radio-link's one_builder test reads every crate's sources, so any crate change runs it.
has rooms 'cargo test --locked -p radio-link --test one_builder'
lacks rooms "$fuzz_test"
lacks rooms 'bash tests/release_test.sh'
lacks rooms 'bash tests/term_check_test.sh'

# The fuzz targets depend on radio-link, so a radio-link change checks them too.
reset
printf '// touched\n' >> "$repo/crates/radio-link/src/lib.rs"
list --tier touched --list || fail "link: exit $?"
has link 'cargo clippy --all-targets --locked -p radio -p radio-link -- -D warnings'
has link "$fuzz_test"
lacks link 'cargo test --locked -p radio-link --test one_builder'
has link 'cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings'

# A new, untracked file counts.
reset
printf '// new\n' > "$repo/crates/radio-client/src/new_file.rs"
list --tier touched --list || fail "untracked: exit $?"
has untracked 'cargo test --locked -p radio -p radio-client'

# Files the crates' tests read count as those crates.
reset
printf 'x\n' >> "$repo/README.md"
list --tier touched --list || fail "readme: exit $?"
has readme 'cargo test --locked -p radio'
lacks readme radio-client
reset
printf 'x\n' >> "$repo/CONTRACT.md"
list --tier touched --list || fail "contract: exit $?"
has contract 'cargo test --locked -p radio -p radio-client'

# A shell script runs its own test and no Rust check.
reset
printf '# x\n' >> "$repo/scripts/release.sh"
list --tier touched --list || fail "release: exit $?"
always release
has release 'bash tests/release_test.sh'
lacks release 'cargo test'
lacks release 'cargo clippy'
reset
printf '# x\n' >> "$repo/tests/ci_local_test.sh"
printf '# x\n' >> "$repo/scripts/ci-local.sh"
list --tier touched --list || fail "self: exit $?"
has self 'bash tests/ci_local_test.sh'
[ "$(grep -c -F -x 'bash tests/ci_local_test.sh' "$tmp/out")" -eq 1 ] || fail 'self: the test is listed twice'
lacks self 'bash tests/release_test.sh'

# A document no check reads runs only the repo-wide scans.
reset
printf 'x\n' > "$repo/NOTES.md"
list --tier touched --list || fail "docs: exit $?"
always docs
lacks docs 'cargo test'
lacks docs 'cargo clippy'

# The lock file, or a path the tier does not know, runs the full tier.
for path in Cargo.lock unknown.txt; do
    reset
    printf '\n' >> "$repo/$path"
    list --tier touched --list || fail "$path: exit $?"
    has "$path" 'cargo clippy --all-targets --locked -- -D warnings'
    has "$path" 'cargo test --locked'
    has "$path" 'cargo test --locked -p radio-client -p radio-rooms'
    has "$path" "$fuzz_test"
    has "$path" 'bash tests/release_test.sh'
    has "$path" 'bash tests/term_check_test.sh'
    has "$path" 'bash tests/ci_local_test.sh'
done

# No change at all still runs the repo-wide scans.
reset
list --tier touched --list || fail "clean: exit $?"
always clean
lacks clean 'cargo test'

# A base that does not resolve runs the full tier rather than nothing.
reset
(cd "$repo" && RADIO_CI_BASE=no-such-ref bash scripts/ci-local.sh --tier touched --list > "$tmp/out" 2> "$tmp/err") \
    || fail "no base: exit $?"
has 'no base' 'cargo test --locked'
grep -F -q 'no-such-ref' "$tmp/err" || fail 'no base: the base is not named'

# A dependents query that fails or finds nothing runs the full tier rather than no Rust check.
for mode in fail empty; do
    reset
    printf '// touched\n' >> "$repo/crates/radio-rooms/src/lib.rs"
    STUB_TREE=$mode list --tier touched --list || fail "tree $mode: exit $?"
    has "tree $mode" 'cargo test --locked'
done

# A base that reads as an option is refused, and runs the full tier.
reset
(cd "$repo" && RADIO_CI_BASE=--all bash scripts/ci-local.sh --tier touched --list > "$tmp/out" 2> "$tmp/err") \
    || fail "option base: exit $?"
has 'option base' 'cargo test --locked'

# The full tier lists every step, the whole workspace at once.
reset
list --list || fail "full: exit $?"
always full
has full 'cargo test --locked'
has full 'cargo test --locked -p radio-client -p radio-rooms'
has full "$fuzz_test"
has full 'bash tests/ci_local_test.sh'

# Bad arguments are refused.
for args in '--tier' '--tier fast' '--bogus'; do
    # shellcheck disable=SC2086
    (cd "$repo" && bash scripts/ci-local.sh $args > /dev/null 2>&1)
    status=$?
    [ "$status" -eq 2 ] || fail "args '$args': exit $status, want 2"
done

if [ -s "$tmp/calls/ran" ]; then
    fail "--list ran a check: $(head -n 3 "$tmp/calls/ran")"
fi
if [ "$failed" -ne 0 ]; then
    exit 1
fi
echo 'ci_local_test: all passed'
