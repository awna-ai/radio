#!/usr/bin/env bash
set -u -o pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
tmp=$(mktemp -d)
tmp=$(cd "$tmp" && pwd -P)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/calls"
export STUB_ROOT="$tmp"
STUB_GIT=$(command -v git)
export STUB_GIT
export PATH="$tmp/bin:$PATH"
term_title=Bramble'wick'
printf '# invented terms\n\n  # comment\n%s  \r\n' "$term_title" > "$tmp/terms.txt"
failed=0

# Refuse any accidental remote lookup, including while testing the old script.
cat > "$tmp/bin/git" <<'STUB'
#!/usr/bin/env bash
for arg in "$@"; do
    if [ "$arg" = ls-remote ]; then
        printf '%s\n' "$*" >> "$STUB_ROOT/calls/git-remote"
        exit 1
    fi
done
exec "$STUB_GIT" "$@"
STUB
cat > "$tmp/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/cargo"
if [ "$1" = pkgid ]; then
    version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml)
    printf 'path+file://%s/crates/radio#%s\n' "$PWD" "$version"
elif [ "$1" = metadata ]; then
    case "${STUB_METADATA:-}" in
        fail) exit 101 ;;
        empty) printf '{"packages":[]}\n'; exit 0 ;;
        noradio) printf '{"packages":[{"name":"other","targets":[{"kind":["lib"],"crate_types":["lib"],"name":"other"}]}]}\n'; exit 0 ;;
    esac
    printf '{"packages":[{"name":"radio","targets":[{"kind":["bin"],"crate_types":["bin"],"name":"radio","src_path":"x"},{"kind":["test"],"crate_types":["bin"],"name":"zztest"}]}'
    for crate in ${STUB_CRATES:-}; do
        printf ',{"name":"%s","targets":[{"kind":["lib"],"crate_types":["lib"],"name":"%s"}]}' "$crate" "$crate"
    done
    for crate in ${STUB_CDYLIB_CRATES:-}; do
        printf ',{"name":"%s","targets":[{"kind":["lib","cdylib"],"crate_types":["lib","cdylib"],"name":"%s"}]}' "$crate" "$crate"
    done
    for crate in ${STUB_CDYLIB_ONLY_CRATES:-}; do
        printf ',{"name":"%s","targets":[{"kind":["cdylib"],"crate_types":["cdylib"],"name":"%s"}]}' "$crate" "$crate"
    done
    printf ']}\n'
else
    git rev-parse HEAD >> "$STUB_ROOT/calls/cargo-head"
    target='' dest=''
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --target) target=$2; shift ;;
            --target-dir) dest=$2; shift ;;
        esac
        shift
    done
    case "$target" in
        aarch64-apple-darwin) arch=arm64 ;;
        x86_64-apple-darwin) arch=x86_64 ;;
        *) exit 1 ;;
    esac
    mkdir -p "$dest/$target/release"
    printf 'arch=%s\n%s\n' "$arch" "${STUB_BINARY_TEXT:-}" > "$dest/$target/release/radio"
    if [ -n "${STUB_BINARY_TRIE:-}" ]; then
        printf '\001%s\002\n' "$STUB_BINARY_TRIE" >> "$dest/$target/release/radio"
    fi
    printf '%s\n' "$dest" >> "$STUB_ROOT/calls/cargo-dirs"
    if [ "${STUB_EMBED_WORK:-0}" = 1 ]; then
        dirname "$dest" >> "$dest/$target/release/radio"
    fi
    if [ -n "${STUB_BINARY_TAIL:-}" ]; then
        printf '%s' "$STUB_BINARY_TAIL" >> "$dest/$target/release/radio"
    fi
    xattr -w com.example.test 1 "$dest/$target/release/radio"
    chmod 755 "$dest/$target/release/radio"
    while IFS= read -r variable; do
        case "$variable" in
            RUSTC|CARGO_PROFILE_*|CARGO_BUILD_*|CARGO_TARGET_*)
                printf '%s\n' "$variable" >> "$STUB_ROOT/calls/cargo-overrides" ;;
        esac
    done < <(compgen -e)
    flags=${CARGO_ENCODED_RUSTFLAGS:-}
    printf 'RUSTFLAGS=%s; CARGO_ENCODED_RUSTFLAGS=%s\n' "${RUSTFLAGS:-}" "${flags//$'\x1f'/ | }" >> "$STUB_ROOT/calls/cargo-env"
fi
STUB
cat > "$tmp/bin/rustup" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/rustup"
printf '%s\n' "${STUB_TARGETS-aarch64-apple-darwin x86_64-apple-darwin}" | tr ' ' '\n'
STUB
cat > "$tmp/bin/lipo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/lipo"
if [ -n "${STUB_LIPO_ARCH:-}" ]; then
    printf '%s\n' "$STUB_LIPO_ARCH"
else
    sed -n 's/^arch=//p' "$2"
fi
STUB
cat > "$tmp/bin/codesign" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/codesign"
STUB
cat > "$tmp/bin/strip" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/strip"
[ "${STUB_STRIP_FAIL:-0}" != 1 ] || exit 1
[ "$#" -eq 2 ] && [ "$1" = -S ] || exit 1
sed '/^debugmap=/d' "$2" > "$STUB_ROOT/stripped"
cat "$STUB_ROOT/stripped" > "$2"
chmod 755 "$2"
STUB
cat > "$tmp/bin/grep" <<'STUB'
#!/usr/bin/env bash
for arg in "$@"; do
    case "${STUB_GREP_FAIL:-0}:$arg" in
        1:-a|terms:-f|'runs:[[:print:]]{4,}') exit 2 ;;
        'noruns:[[:print:]]{4,}') exit 1 ;;
    esac
done
exec /usr/bin/grep "$@"
STUB
# Real binaries keep the export trie in LC_DYLD_INFO_ONLY (x86_64 here) or LC_DYLD_EXPORTS_TRIE (arm64 here).
cat > "$tmp/bin/otool" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/otool"
[ "${STUB_OTOOL:-}" != fail ] || exit 1
[ "$#" -eq 2 ] && [ "$1" = -l ] || exit 1
off=0 size=0
if [ -n "${STUB_BINARY_TRIE:-}" ]; then
    off=$(LC_ALL=C /usr/bin/grep -a -b -o -F -- "$STUB_BINARY_TRIE" "$2" | head -1 | cut -d: -f1)
    size=$(printf '%s' "$STUB_BINARY_TRIE" | wc -c | tr -d ' ')
fi
case "${STUB_OTOOL:-}" in
    none) printf 'Load command 0\n      cmd LC_SEGMENT_64\n  cmdsize 72\n'; exit 0 ;;
    beyond) off=$(wc -c < "$2" | tr -d ' ') size=1 ;;
    word) size=many ;;
esac
printf 'Load command 0\n      cmd LC_SEGMENT_64\n  cmdsize 72\n'
if /usr/bin/grep -q '^arch=x86_64$' "$2"; then
    printf 'Load command 4\n            cmd LC_DYLD_INFO_ONLY\n        cmdsize 48\n     rebase_off 4\n    rebase_size 8\n'
    printf '     export_off %s\n    export_size %s\n' "$off" "$size"
else
    printf 'Load command 4\n      cmd LC_DYLD_EXPORTS_TRIE\n  cmdsize 16\n  dataoff %s\n datasize %s\n' "$off" "$size"
fi
printf 'Load command 5\n      cmd LC_SYMTAB\n  cmdsize 24\n   symoff 8\n'
STUB
cat > "$tmp/bin/strings" <<'STUB'
#!/usr/bin/env bash
# A scan that relied on strings would miss every term.
exit 0
STUB
cat > "$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_ROOT/calls/gh"
printf 'GH_REPO=%s\n' "${GH_REPO:-}" >> "$STUB_ROOT/calls/gh-env"
if [ "$1" = api ]; then
    endpoint=$2
    repository=${endpoint#repos/}
    repository=${repository%%/git/*}
    printf '%s\n' "$repository" >> "$STUB_ROOT/calls/api-repos"
    case "$endpoint" in
        repos/*/git/ref/tags/*)
            tag=${endpoint#*/git/ref/tags/}
            if ! sha=$(git -C "$STUB_ORIGIN" rev-parse --verify -q "refs/tags/$tag"); then
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            fi
            printf '%s %s\n' "$(git -C "$STUB_ORIGIN" cat-file -t "$sha")" "$sha"
            ;;
        repos/*/git/tags/*)
            sha=${endpoint#*/git/tags/}
            git -C "$STUB_ORIGIN" cat-file tag "$sha" | awk '
                /^object / { object=$2 }
                /^type / { type=$2 }
                /^$/ { print type " " object; exit }
            '
            ;;
        *) exit 1 ;;
    esac
    exit 0
fi
case "$2" in
    view)
        if [[ " $* " == *' --json assets '* ]]; then
            find "$STUB_ROOT/server" -type f -exec basename {} \;
            if [ "${STUB_GH_EXTRA_ASSET:-0}" = 1 ]; then echo extra.txt; fi
            exit 0
        fi
        if [[ " $* " == *' --json isDraft '* ]]; then
            if [ "${STUB_GH_STILL_DRAFT:-0}" = 1 ]; then echo true; else echo false; fi
            exit 0
        fi
        if [ "${STUB_GH_VIEW_ERROR:-0}" = 1 ]; then
            echo 'HTTP 401' >&2
            exit 1
        fi
        [ "${STUB_GH_EXISTS:-0}" = 1 ] && exit 0
        echo 'release not found' >&2
        exit 1
        ;;
    create)
        mkdir -p "$STUB_ROOT/server"
        shift 3
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --verify-tag|--draft) ;;
                -R) shift ;;
                --title) shift ;;
                --notes-file) cp "$2" "$STUB_ROOT/notes.md"; shift ;;
                *) cp "$1" "$STUB_ROOT/server/" ;;
            esac
            shift
        done
        ;;
    edit) ;;
    download)
        tag=$3
        shift 3
        dest=''
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --dir) dest=$2; shift ;;
                --pattern) shift ;;
            esac
            shift
        done
        if [ "${STUB_GH_MOVE_TAG:-0}" = 1 ]; then
            tree=$(git -C "$STUB_ORIGIN" rev-parse 'HEAD^{tree}')
            moved=$(git -C "$STUB_ORIGIN" commit-tree "$tree" -m moved)
            git -C "$STUB_ORIGIN" tag -f -a -m moved "$tag" "$moved" > /dev/null
        fi
        mkdir -p "$dest"
        cp "$STUB_ROOT/server/"* "$dest/"
        if [ "${STUB_GH_CORRUPT:-0}" = 1 ]; then
            printf x >> "$dest/SHA256SUMS"
        fi
        if [ "${STUB_GH_CORRUPT_ARCHIVE:-0}" = 1 ]; then
            printf x >> "$dest/radio-v0.1.0-macos-arm64.tar.gz"
        fi
        if [ "${STUB_GH_DROP:-0}" = 1 ]; then
            rm "$dest/radio-v0.1.0-macos-x86_64.tar.gz"
        fi
        if [ "${STUB_GH_EXTRA:-0}" = 1 ]; then
            printf x > "$dest/radio-extra.txt"
        fi
        ;;
    *) exit 1 ;;
esac
STUB
chmod 755 "$tmp/bin/"*

fail() {
    printf 'release_test: FAIL %s: %s\n' "$case_number" "$1"
    failed=1
}

new_case() {
    case_number=$1
    repo="$tmp/case-$case_number"
    mkdir -p "$repo/scripts"
    rm -rf "$tmp/calls" "$tmp/server"
    rm -rf "$tmp/tmpdir"
    mkdir "$tmp/calls" "$tmp/tmpdir"
    export TMPDIR="$tmp/tmpdir"
    unset STUB_ORIGIN STUB_GH_EXTRA_ASSET STUB_GH_MOVE_TAG
    unset CARGO_HOME RUSTUP_HOME RUSTC CARGO_BUILD_JOBS CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER CARGO_PROFILE_RELEASE_LTO
    unset STUB_EMBED_WORK STUB_GREP_FAIL STUB_GH_STILL_DRAFT STUB_GH_VIEW_ERROR
    unset STUB_GH_CORRUPT_ARCHIVE STUB_GH_DROP STUB_GH_EXTRA GH_REPO
    unset STUB_BINARY_TEXT STUB_BINARY_TAIL STUB_TARGETS STUB_LIPO_ARCH STUB_GH_EXISTS STUB_GH_CORRUPT STUB_STRIP_FAIL
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS STUB_BINARY_TRIE STUB_OTOOL STUB_METADATA STUB_CRATES
    unset STUB_CDYLIB_CRATES STUB_CDYLIB_ONLY_CRATES
    export RADIO_TERMS_FILE="$tmp/terms.txt"
    git init -q -b main "$repo" || exit 1
    git -C "$repo" config user.name Test
    git -C "$repo" config user.email test@example.invalid
    git -C "$repo" config commit.gpgsign false
    printf '[workspace.package]\nversion = "0.1.0"\n' > "$repo/Cargo.toml"
    printf 'MIT test licence v1\n' > "$repo/LICENSE"
    if [ -f "$root/scripts/release.sh" ]; then
        cp "$root/scripts/release.sh" "$repo/scripts/release.sh"
    fi
    git -C "$repo" add .
    git -C "$repo" commit -qm 'add source' || exit 1
    git -C "$repo" tag -a -m v0.1.0 v0.1.0 || exit 1
    printf 'MIT test licence v2\n' > "$repo/LICENSE"
    git -C "$repo" commit -qam 'update licence' || exit 1
    printf 'scratch\n' > "$repo/scratch.txt"
    out="$repo/target/release-dist/v0.1.0"
}

run() {
    output=$(bash "$repo/scripts/release.sh" "$@" 2>&1)
    run_status=$?
    [ -z "$(find "$tmp/tmpdir" -mindepth 1 -print)" ] || fail 'temporary files not removed'
    [ "$(git -C "$repo" worktree list | wc -l)" -eq 1 ] || fail 'worktree not removed'
}

expect() {
    local expected=$1 wanted=$2 actual
    shift 2
    run "$@"
    actual=$run_status
    [ "$actual" -eq "$expected" ] || fail "expected exit $expected, got $actual"
    printf '%s\n' "$output" | grep -F -q -- "$wanted" || fail 'expected diagnostic absent'
    if printf '%s\n' "$output" | grep -F -i -q -- "$term_title"; then
        fail 'output exposed a private term'
    fi
}

refused() {
    expect "$@"
    [ ! -e "$out" ] || fail 'refused build created a dist'
}

origin() {
    git init --bare -q "$tmp/origin-$case_number.git" || exit 1
    git -C "$repo" remote add origin git@github.com:example-org/example-repo.git
    export STUB_ORIGIN="$tmp/origin-$case_number.git"
    git -C "$STUB_ORIGIN" config user.name Test
    git -C "$STUB_ORIGIN" config user.email test@example.invalid
    git -C "$STUB_ORIGIN" config commit.gpgsign false
    git -C "$STUB_ORIGIN" symbolic-ref HEAD refs/heads/main
    git -C "$repo" push -q "$STUB_ORIGIN" main || exit 1
    if [ "$1" = tag ]; then
        git -C "$repo" push -q "$STUB_ORIGIN" v0.1.0 || exit 1
    fi
}

no_create() {
    if [ -f "$tmp/calls/gh" ] && grep -q '^release create ' "$tmp/calls/gh"; then
        fail 'created a release after refusal'
    fi
}

no_build() {
    if [ -f "$tmp/calls/cargo" ] && grep -q '^build ' "$tmp/calls/cargo"; then
        fail 'built after publish refusal'
    fi
}

no_edit() {
    if [ -f "$tmp/calls/gh" ] && grep -q '^release edit ' "$tmp/calls/gh"; then
        fail 'published before successful read-back'
    fi
}

new_case 1
refused 2 usage:
refused 2 usage: v1.2
refused 2 usage: v01.2.3
refused 2 usage: v0.1.0 --bogus
refused 2 usage: v0.1.0 --publish extra

new_case 2
refused 1 'release: no tag v9.9.9' v9.9.9

new_case 3
git -C "$repo" tag v0.2.0 'v0.1.0^{commit}'
refused 1 'is not the workspace version 0.1.0' v0.2.0
[ ! -e "$repo/target/release-dist/v0.2.0" ] || fail 'refused version created a dist'

new_case 4
export STUB_TARGETS=aarch64-apple-darwin
refused 1 'rustup target add x86_64-apple-darwin' v0.1.0
if [ -f "$tmp/calls/cargo" ] && grep -q '^build ' "$tmp/calls/cargo"; then
    fail 'built with a missing target'
fi

new_case 5
export RADIO_TERMS_FILE="$tmp/absent.txt"
refused 1 'release: no term list' v0.1.0

new_case 6
export RUSTFLAGS=--bogus
export CARGO_HOME="$tmp/cargo-home" RUSTUP_HOME="$tmp/rustup-home"
export RUSTC=bogus CARGO_BUILD_JOBS=1 CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=bogus CARGO_PROFILE_RELEASE_LTO=true
expect 0 'release: to publish it, run: bash scripts/release.sh v0.1.0 --publish' v0.1.0
printf '%s\n' radio-v0.1.0-macos-arm64.tar.gz radio-v0.1.0-macos-x86_64.tar.gz SHA256SUMS | sort > "$tmp/expected"
if [ -d "$out" ]; then
    (cd "$out" && find . -type f | sed 's|^./||' | sort) > "$tmp/actual"
    cmp -s "$tmp/expected" "$tmp/actual" || fail 'dist file names differ'
    (cd "$out" && shasum -a 256 -c SHA256SUMS) > /dev/null 2>&1 || fail 'checksums differ'
    for arch in arm64 x86_64; do
        folder="radio-v0.1.0-macos-$arch"
        archive="$folder.tar.gz"
        printf '%s\n' "$output" | grep -F -q -- "$archive" || fail 'archive absent from output'
        printf '%s\n' "$folder/" "$folder/LICENSE" "$folder/radio" | sort > "$tmp/expected"
        for pattern in '._' com.example.test xattr; do
            count=$(gzip -dc "$out/$archive" | LC_ALL=C grep -a -F -c -- "$pattern")
            [ "$count" -eq 0 ] || fail 'archive holds extended metadata'
        done
        tar -tzf "$out/$archive" | sort > "$tmp/actual"
        cmp -s "$tmp/expected" "$tmp/actual" || fail 'archive entries differ'
        tar -tvzf "$out/$archive" > "$tmp/listing"
        [ "$(grep -c 'root[[:space:]]*wheel' "$tmp/listing")" -eq 3 ] || fail 'archive owner differs'
        grep -q "^-rwxr-xr-x.*$folder/radio$" "$tmp/listing" || fail 'binary mode differs'
        grep -q "^drwxr-xr-x.*$folder/$" "$tmp/listing" || fail 'folder mode differs'
        grep -q "^-rw-r--r--.*$folder/LICENSE$" "$tmp/listing" || fail 'licence mode differs'
        mkdir "$tmp/extract-$arch"
        tar -xzf "$out/$archive" -C "$tmp/extract-$arch"
        [ "$(cat "$tmp/extract-$arch/$folder/LICENSE")" = 'MIT test licence v1' ] || fail 'licence is not from tag'
        grep -q "^arch=$arch$" "$tmp/extract-$arch/$folder/radio" || fail 'binary architecture differs'
    done
else
    fail 'dist absent'
fi
printf '%s\n' "$output" | grep -F -q SHA256SUMS || fail 'checksum file absent from output'
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    if [ ! -f "$tmp/calls/cargo" ] || ! grep -q "^build --release --locked -p radio --target $target --target-dir " "$tmp/calls/cargo"; then
        fail 'build flags differ'
    fi
done
if [ -f "$tmp/calls/cargo-head" ]; then
    tag_commit=$(git -C "$repo" rev-parse 'v0.1.0^{commit}')
    [ "$(grep -F -x -c "$tag_commit" "$tmp/calls/cargo-head")" -eq 2 ] || fail 'build is not from tag'
else
    fail 'build source absent'
fi
if [ -f "$tmp/calls/cargo-env" ]; then
    [ "$(wc -l < "$tmp/calls/cargo-env")" -eq 2 ] || fail 'build environment count differs'
    [ "$(grep -c '^RUSTFLAGS=; ' "$tmp/calls/cargo-env")" -eq 2 ] || fail 'inherited rust flags'
    build_work=$(dirname "$(head -1 "$tmp/calls/cargo-dirs")")
    expected_flags="RUSTFLAGS=; CARGO_ENCODED_RUSTFLAGS=--remap-path-prefix=$HOME=/home | --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo | --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup | --remap-path-prefix=$build_work=/build"
    [ "$(grep -F -x -c -- "$expected_flags" "$tmp/calls/cargo-env")" -eq 2 ] || fail 'remap flags differ'
else
    fail 'build environment absent'
fi
if [ ! -f "$tmp/calls/codesign" ] || [ "$(grep -c -- '--sign - --identifier ai.awna.radio' "$tmp/calls/codesign")" -ne 2 ]; then
    fail 'ad hoc signing calls differ'
fi
if [ -f "$tmp/calls/strip" ]; then
    [ "$(wc -l < "$tmp/calls/strip")" -eq 2 ] || fail 'strip call count differs'
    sed -E 's|^-S .*/target/|-S |' "$tmp/calls/strip" > "$tmp/actual"
    printf '%s\n' '-S aarch64-apple-darwin/release/radio' '-S x86_64-apple-darwin/release/radio' > "$tmp/expected"
    cmp -s "$tmp/expected" "$tmp/actual" || fail 'strip arguments differ'
else
    fail 'strip calls absent'
fi
if [ -f "$tmp/calls/otool" ]; then
    sed -E 's|^-l .*/target/|-l |' "$tmp/calls/otool" > "$tmp/actual"
    printf '%s\n' '-l aarch64-apple-darwin/release/radio' '-l x86_64-apple-darwin/release/radio' > "$tmp/expected"
    cmp -s "$tmp/expected" "$tmp/actual" || fail 'otool arguments differ'
else
    fail 'otool calls absent'
fi
grep -F -x -q 'metadata --locked --offline --format-version 1 --filter-platform aarch64-apple-darwin --filter-platform x86_64-apple-darwin' "$tmp/calls/cargo" || fail 'metadata arguments differ'
[ ! -f "$tmp/calls/cargo-overrides" ] || fail 'inherited build overrides'
[ ! -f "$tmp/calls/gh" ] || fail 'local build called gh'
[ "$(git -C "$repo" worktree list --porcelain | grep -c '^worktree ')" -eq 1 ] || fail 'worktree not removed'
[ -f "$repo/scratch.txt" ] || fail 'dirty file removed'

new_case 7
export STUB_BINARY_TEXT="$HOME/src/lib.rs"
refused 1 'holds a build path' v0.1.0
if printf '%s\n' "$output" | grep -F -q -- "$HOME/src"; then
    fail 'output exposed the build path'
fi

new_case 8
export STUB_BINARY_TEXT="the ${term_title}App"
refused 1 'names a private term' v0.1.0

new_case 9
export STUB_LIPO_ARCH=x86_64
refused 1 'release: the aarch64-apple-darwin binary is not arm64' v0.1.0

new_case 10
expect 0 'release: built v0.1.0' v0.1.0
if [ -f "$out/SHA256SUMS" ]; then
    cp "$out/SHA256SUMS" "$tmp/prior-sums"
    export STUB_BINARY_TEXT="$HOME/src/lib.rs"
    expect 1 'holds a build path' v0.1.0
    cmp -s "$tmp/prior-sums" "$out/SHA256SUMS" || fail 'failed build changed prior dist'
else
    fail 'prior dist absent'
fi

new_case 11
origin main
expect 0 'release: built v0.1.0' v0.1.0
cp "$out/SHA256SUMS" "$tmp/prior-sums"
builds_before=$(grep -c '^build ' "$tmp/calls/cargo")
expect 1 'is not on example-org/example-repo at' v0.1.0 --publish
cmp -s "$tmp/prior-sums" "$out/SHA256SUMS" || fail 'publish refusal changed prior dist'
[ "$(grep -c '^build ' "$tmp/calls/cargo")" -eq "$builds_before" ] || fail 'built after tag refusal'
no_create

new_case 12
origin tag
export STUB_GH_EXISTS=1
expect 1 'already has a release' v0.1.0 --publish
no_create

new_case 13
origin tag
export GH_REPO=other/repo
expect 0 'release: published v0.1.0 to example-org/example-repo (3 files, read back)' v0.1.0 --publish
grep -F -x -A 1 'Signed ad hoc, not notarised. Ship it inside your app, whose signature and notarisation cover it; see' "$tmp/notes.md" |
    grep -F -x -q '*For app developers* in the README.' || fail 'notes README pointer differs'
if grep -F -q -e 'docs/' -e 'app-developers.md' "$tmp/notes.md"; then
    fail 'notes name the removed guide'
fi
if [ -f "$tmp/calls/gh" ]; then
    [ ! -f "$tmp/calls/git-remote" ] || fail 'used ls-remote'
    grep -F -q "release create v0.1.0 -R example-org/example-repo --draft --verify-tag --title radio v0.1.0 --notes-file " "$tmp/calls/gh" || fail 'create options differ'
    create_line=$(sed -n '/^release create /p' "$tmp/calls/gh")
    upload_work=$(printf '%s\n' "$create_line" | sed -n 's|.*--notes-file \(.*\)/notes.md .*|\1|p')
    [[ "$upload_work" == "$tmp/tmpdir/"* ]] || fail 'upload is not from the private temp folder'
    printf '%s\n' "$create_line" | grep -F -q "$upload_work/dist/radio-v0.1.0-macos-arm64.tar.gz $upload_work/dist/radio-v0.1.0-macos-x86_64.tar.gz $upload_work/dist/SHA256SUMS" || fail 'upload paths or order differ'
    [ "$(grep -v '^api ' "$tmp/calls/gh" | grep -F -c -- '-R example-org/example-repo')" -eq "$(grep -v -c '^api ' "$tmp/calls/gh")" ] || fail 'gh repository differs'
    [ -f "$tmp/calls/api-repos" ] && [ "$(grep -F -x -c example-org/example-repo "$tmp/calls/api-repos")" -eq "$(wc -l < "$tmp/calls/api-repos")" ] || fail 'api repository differs'
    [ "$(grep -c '^api repos/example-org/example-repo/git/ref/tags/v0.1.0 ' "$tmp/calls/gh")" -eq 2 ] || fail 'tag lookup count differs'
    grep -F -x -q 'release edit v0.1.0 -R example-org/example-repo --draft=false' "$tmp/calls/gh" || fail 'draft publish absent'
    download_line=$(sed -n '/^release download /=' "$tmp/calls/gh")
    edit_line=$(sed -n '/^release edit /=' "$tmp/calls/gh")
    [ -n "$edit_line" ] && [ "$edit_line" -gt "$download_line" ] || fail 'published before download'
    tag_line=$(sed -n '\|^api repos/example-org/example-repo/git/ref/tags/v0.1.0 |=' "$tmp/calls/gh" | tail -1)
    [ -n "$tag_line" ] && [ "$tag_line" -gt "$download_line" ] && [ "$tag_line" -lt "$edit_line" ] || fail 'tag not rechecked before publishing'
    [ "$(find "$tmp/server" -type f | wc -l)" -eq 3 ] || fail 'uploaded file count differs'
    [ "$(grep -c '^GH_REPO=$' "$tmp/calls/gh-env")" -eq "$(wc -l < "$tmp/calls/gh-env")" ] || fail 'inherited gh repository'
else
    fail 'publish calls absent'
fi

new_case 14
origin tag
export STUB_GH_CORRUPT=1
expect 1 'did not read back' v0.1.0 --publish
no_edit
printf '%s\n' "$output" | grep -F -q 'gh release delete' || fail 'draft removal command absent'

new_case 15
export STUB_BINARY_TEXT="debugmap=$HOME/lib/libstd.rlib"
expect 0 'release: built v0.1.0' v0.1.0
printf '%s\n' radio-v0.1.0-macos-arm64.tar.gz radio-v0.1.0-macos-x86_64.tar.gz SHA256SUMS | sort > "$tmp/expected"
if [ -d "$out" ]; then
    (cd "$out" && find . -type f | sed 's|^./||' | sort) > "$tmp/actual"
    cmp -s "$tmp/expected" "$tmp/actual" || fail 'dist file names differ'
else
    fail 'dist absent'
fi

new_case 16
export STUB_STRIP_FAIL=1
refused 1 'release: could not strip the aarch64-apple-darwin binary' v0.1.0
[ ! -f "$tmp/calls/codesign" ] || fail 'signed after strip failed'

new_case 17
STUB_BINARY_TEXT="built by $(id -un)"
export STUB_BINARY_TEXT
refused 1 'release: the aarch64-apple-darwin binary names this account' v0.1.0

new_case 18
export STUB_GREP_FAIL=1
refused 1 'could not scan' v0.1.0
export STUB_GREP_FAIL=terms
refused 1 'could not scan' v0.1.0

new_case 19
export STUB_BINARY_TEXT="$repo/x"
refused 1 'holds a build path' v0.1.0

new_case 20
export STUB_EMBED_WORK=1
refused 1 'holds a build path' v0.1.0

new_case 21
export STUB_BINARY_TEXT="value${term_title}disable"
refused 1 'names a private term' v0.1.0

new_case 22
export STUB_BINARY_TEXT=BRAMBLE'wick'
refused 1 'names a private term' v0.1.0

new_case 23
STUB_BINARY_TEXT="builtby$(id -un)x"
export STUB_BINARY_TEXT
refused 1 'names this account' v0.1.0
STUB_BINARY_TEXT="builtby$(id -un | tr '[:lower:]' '[:upper:]')x"
export STUB_BINARY_TEXT
refused 1 'names this account' v0.1.0

new_case 24
printf '%s\nzq\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=xzqx
expect 0 'release: built v0.1.0' v0.1.0
export STUB_BINARY_TEXT='a zq b'
expect 1 'names a private term' v0.1.0

new_case 25
origin tag
export STUB_GH_VIEW_ERROR=1
refused 1 'could not ask GitHub' v0.1.0 --publish
no_create
no_build

new_case 26
origin tag
export STUB_GH_CORRUPT_ARCHIVE=1
expect 1 'did not read back' v0.1.0 --publish
no_edit

new_case 27
origin tag
export STUB_GH_DROP=1
expect 1 'did not read back' v0.1.0 --publish
no_edit

new_case 28
origin tag
export STUB_GH_STILL_DRAFT=1
expect 1 'is still a draft' v0.1.0 --publish

new_case 29
origin tag
git -C "$repo" config remote.origin.url https://github.com/example-org/example-repo
expect 0 'release: published v0.1.0 to example-org/example-repo' v0.1.0 --publish
grep -F -q -- '-R example-org/example-repo' "$tmp/calls/gh" || fail 'gh repository absent'

new_case 30
origin tag
git -C "$repo" config remote.origin.url git@gitlab.example:example-org/example-repo.git
refused 1 'origin is not a GitHub repository' v0.1.0 --publish
no_build

new_case 31
origin tag
export STUB_GH_EXISTS=1
refused 1 'gh release delete v0.1.0 -R example-org/example-repo --yes' v0.1.0 --publish
no_build

# A missing file also fails cmp; an extra file specifically tests the exact list.
new_case 32
origin tag
export STUB_GH_EXTRA=1
expect 1 'did not read back' v0.1.0 --publish
no_edit

new_case 33
printf 'zq\n' > "$tmp/normalized-terms.txt"
export RADIO_TERMS_FILE="$tmp/normalized-terms.txt" STUB_BINARY_TEXT=ZqThing
refused 1 'names a private term' v0.1.0

new_case 34
printf '   %s\n' "$term_title" > "$tmp/indented-terms.txt"
export RADIO_TERMS_FILE="$tmp/indented-terms.txt" STUB_BINARY_TEXT="$term_title"
refused 1 'names a private term' v0.1.0

new_case 35
origin tag
export STUB_GH_MOVE_TAG=1
expect 1 'is not on example-org/example-repo at' v0.1.0 --publish
printf '%s\n' "$output" | grep -F -q 'gh release delete' || fail 'draft removal command absent'
no_edit

new_case 36
origin tag
export STUB_GH_EXTRA_ASSET=1
expect 1 'did not read back' v0.1.0 --publish
no_edit

new_case 37
origin tag
tree=$(git -C "$STUB_ORIGIN" rev-parse 'HEAD^{tree}')
moved=$(git -C "$STUB_ORIGIN" commit-tree "$tree" -m moved)
git -C "$STUB_ORIGIN" tag -f -a -m moved v0.1.0 "$moved" > /dev/null
refused 1 'is not on example-org/example-repo at' v0.1.0 --publish
no_build
no_create

new_case 38
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01zqv\x02'
expect 0 'release: built v0.1.0' v0.1.0

new_case 39
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01call zqv now\x02'
refused 1 'names a private term' v0.1.0

new_case 40
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01callZqvNow\x02'
refused 1 'names a private term' v0.1.0

# A long term in bare bytes is caught; case 45 distinguishes the raw long scan from a text scan.
new_case 41
export STUB_BINARY_TEXT=$'\x01'"$term_title"$'\x02'
refused 1 'names a private term' v0.1.0

new_case 42
export STUB_GREP_FAIL=runs
refused 1 'could not scan' v0.1.0

new_case 43
export STUB_GREP_FAIL=noruns
refused 1 'could not scan' v0.1.0

new_case 44
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TAIL=$'\x01call zqv'
refused 1 'names a private term' v0.1.0

new_case 45
printf '%s\n%s\n' "$term_title" "Fennel"$'\xc3\xa9'"moor" > "$tmp/long-terms.txt"
export RADIO_TERMS_FILE="$tmp/long-terms.txt" STUB_BINARY_TEXT=$'\x01'"Fennel"$'\xc3\xa9'"moor"$'\x02'
refused 1 'names a private term' v0.1.0

new_case 46
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01line one\nzqv\nline two\x02'
refused 1 'names a private term' v0.1.0

new_case 47
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01col\tzqv\tcol\x02'
refused 1 'names a private term' v0.1.0

new_case 48
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01\r\nzqv\r\n\x02'
refused 1 'names a private term' v0.1.0

new_case 49
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_5radio4main\x02'
expect 0 'release: built v0.1.0' v0.1.0

new_case 50
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1a2b_3zqv4main\x02'
refused 1 'names a private term' v0.1.0

new_case 51
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01loadCsZqv_now\x02'
refused 1 'names a private term' v0.1.0

new_case 52
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_u5radio4main\x02'
expect 0 'release: built v0.1.0' v0.1.0

new_case 53
printf '%s\n' "$term_title" > "$tmp/long-terms.txt"
export RADIO_TERMS_FILE="$tmp/long-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1a'"$term_title"$'9_5radio4main\x02'
refused 1 'names a private term' v0.1.0

# The export trie stores names in fragments; a short term there is chance, and each exported name is whole in the symbol table.
new_case 54
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TRIE='edge zqv edge'
expect 0 'release: built v0.1.0' v0.1.0

new_case 55
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TRIE='edge zqv edge' STUB_BINARY_TEXT=$'\x01_call_zqv_now\x02'
refused 1 'names a private term' v0.1.0

new_case 56
export STUB_BINARY_TRIE="edge ${term_title} edge"
refused 1 'names a private term' v0.1.0

new_case 57
export STUB_OTOOL=fail
refused 1 'could not read the aarch64-apple-darwin export trie' v0.1.0

new_case 58
export STUB_OTOOL=none
refused 1 'could not read the aarch64-apple-darwin export trie' v0.1.0

new_case 59
export STUB_OTOOL=beyond
refused 1 'could not read the aarch64-apple-darwin export trie' v0.1.0

new_case 60
export STUB_OTOOL=word
refused 1 'could not read the aarch64-apple-darwin export trie' v0.1.0

# A crate hash is dropped only before a crate of this build, so a crate missing from the list keeps its hash.
new_case 61
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_7zzother4main\x02'
refused 1 'names a private term' v0.1.0
export STUB_CRATES='zzfirst zzother'
expect 0 'release: built v0.1.0' v0.1.0

new_case 62
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_6zztest4main\x02'
refused 1 'names a private term' v0.1.0
export STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_3std2io\x02'
expect 0 'release: built v0.1.0' v0.1.0

new_case 63
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9abcdef_5radio4main\x02'
refused 1 'names a private term' v0.1.0
export STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9abcde_5radio4main\x02'
expect 0 'release: built v0.1.0' v0.1.0

# A Cs inside a type name is not a crate hash.
new_case 64
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_BINARY_TEXT=$'\x01getCsvZqv_3x\x02'
refused 1 'names a private term' v0.1.0
export STUB_BINARY_TEXT=$'\x01CsZqvCs1_2\x02'
refused 1 'names a private term' v0.1.0

new_case 65
export STUB_METADATA=fail
refused 1 'could not read the crates of this build' v0.1.0
no_build
export STUB_METADATA=empty
refused 1 'could not read the crates of this build' v0.1.0
export STUB_METADATA=noradio
refused 1 'could not read the crates of this build' v0.1.0
unset STUB_METADATA
export STUB_CRATES='zzfine bad.name'
refused 1 'could not read the crates of this build' v0.1.0
export STUB_CRATES='zzfine bad-name'
expect 0 'release: built v0.1.0' v0.1.0

# A real build lists about 250 crates, past what one sed command can hold.
new_case 66
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
STUB_CRATES=$(seq -f 'zzcrate%g' 1 300 | tr '\n' ' ')
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_CRATES STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_10zzcrate3004main\x02'
expect 0 'release: built v0.1.0' v0.1.0

# A lib target that is also a cdylib is a crate of the build; a cdylib alone is not.
new_case 67
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_CDYLIB_ONLY_CRATES=zzcdylib STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_8zzcdylib4main\x02'
refused 1 'names a private term' v0.1.0
unset STUB_CDYLIB_ONLY_CRATES
export STUB_CDYLIB_CRATES=zzcdylib
expect 0 'release: built v0.1.0' v0.1.0

# v0 writes a separator _ before a name that starts with _.
new_case 68
printf '%s\nzqv\n' "$term_title" > "$tmp/short-terms.txt"
export RADIO_TERMS_FILE="$tmp/short-terms.txt" STUB_CRATES=_zzunder STUB_BINARY_TEXT=$'\x01_RNvNtCs1aZqv9_8__zzunder4main\x02'
expect 0 'release: built v0.1.0' v0.1.0

if [ "$failed" -ne 0 ]; then
    exit 1
fi
printf 'release_test: all passed\n'
