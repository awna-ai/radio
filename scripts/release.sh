#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

usage() {
    echo 'usage: scripts/release.sh <vX.Y.Z> [--publish]' >&2
    exit 2
}
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    usage
fi
tag=$1
[[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || usage
publish=0
if [ "$#" -eq 2 ]; then
    [ "$2" = --publish ] || usage
    publish=1
fi
root=$(cd "$(dirname "$0")/.." && pwd -P)
fail() {
    printf 'release: %s\n' "$1" >&2
    exit 1
}
commit=$(git -C "$root" rev-parse --verify -q "refs/tags/$tag^{commit}") || fail "no tag $tag"
terms=${RADIO_TERMS_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/radio-dev/terms.txt}
[ -f "$terms" ] && [ -r "$terms" ] || fail "no term list at $terms"

# Keep the list private and remove every temporary file, including on a refusal.
list=$(mktemp)
work=''
cleanup() {
    rm -f "$list"
    if [ -n "$work" ]; then
        git -C "$root" worktree remove --force "$work/src" > /dev/null 2>&1 || true
        git -C "$root" worktree prune > /dev/null 2>&1 || true
        rm -rf "$work"
    fi
}
trap cleanup EXIT
sed -e 's/\r$//' -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$terms" > "$list" || fail "no term list at $terms"
[ -s "$list" ] || fail "no term list at $terms"

tools=(cargo rustup lipo strip codesign shasum tar)
if [ "$publish" -eq 1 ]; then
    tools+=(gh)
fi
for tool in "${tools[@]}"; do
    command -v "$tool" > /dev/null 2>&1 || fail "$tool is missing"
done
targets=(aarch64-apple-darwin x86_64-apple-darwin)
arches=(arm64 x86_64)
installed=$(rustup target list --installed 2> /dev/null) || fail 'could not list targets'
missing=0
for target in "${targets[@]}"; do
    if ! printf '%s\n' "$installed" | grep -F -x -q -- "$target"; then
        printf 'release: the %s target is missing; add it with: rustup target add %s\n' "$target" "$target" >&2
        missing=1
    fi
done
[ "$missing" -eq 0 ] || exit 1
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
git -C "$root" worktree add -q --detach "$work/src" "$commit" 2> /dev/null || fail 'could not copy the tag'
cd "$work/src"
id=$(cargo pkgid -p radio 2> /dev/null) || fail 'could not read the workspace version'
version=${id##*[#@]}
[ "v$version" = "$tag" ] || fail "$tag is not the workspace version $version"
unset RUSTFLAGS CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_RUSTFLAGS
CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/home"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"$'\x1f'"--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"$'\x1f'"--remap-path-prefix=$work=/build"
export CARGO_ENCODED_RUSTFLAGS

normalize() {
    sed -E 's/_/ /g; s/([a-z0-9])([A-Z])/\1 \2/g; s/([A-Z])([A-Z][a-z])/\1 \2/g; s/([A-Za-z])([0-9])/\1 \2/g; s/([0-9])([A-Za-z])/\1 \2/g' "$@"
}

# A failed grep is distinct from a clean scan; never print the matched bytes.
check_terms() {
    local file=$1 status
    if grep -a -F -i -w -f "$list" -- "$file" > /dev/null 2>&1; then
        fail "the $target binary names a private term"
    else
        status=$?
        [ "$status" -eq 1 ] || fail "could not scan the $target binary"
    fi
}

mkdir -p "$work/dist" "$work/stage"
account=$(id -un) || fail 'could not read the account name'
for i in 0 1; do
    target=${targets[$i]}
    arch=${arches[$i]}
    cargo build --release --locked -p radio --target "$target" --target-dir "$work/target" >&2 || fail "the $target build failed"
    bin="$work/target/$target/release/radio"
    strip -S "$bin" > /dev/null 2>&1 || fail "could not strip the $target binary"
    actual=$(lipo -archs "$bin" 2> /dev/null) || fail "could not read the $target architecture"
    [ "$actual" = "$arch" ] || fail "the $target binary is not $arch"
    for path in "$HOME" "$work" "$root"; do
        if grep -a -F -q -- "$path" "$bin" > /dev/null 2>&1; then
            fail "the $target binary holds a build path"
        else
            status=$?
            [ "$status" -eq 1 ] || fail "could not scan the $target binary"
        fi
    done
    if grep -a -F -w -q -- "$account" "$bin" > /dev/null 2>&1; then
        fail "the $target binary names this account"
    else
        status=$?
        [ "$status" -eq 1 ] || fail "could not scan the $target binary"
    fi
    check_terms "$bin"
    normalize "$bin" > "$work/normalized" 2> /dev/null || fail "could not scan the $target binary"
    check_terms "$work/normalized"
    codesign --force --sign - --identifier ai.awna.radio "$bin" > /dev/null 2>&1 || fail "could not sign the $target binary"
    codesign --verify --strict "$bin" > /dev/null 2>&1 || fail "could not verify the $target signature"
done

# Both binaries have passed the scans and signature checks before packing.
for i in 0 1; do
    target=${targets[$i]}
    arch=${arches[$i]}
    bin="$work/target/$target/release/radio"
    folder="radio-$tag-macos-$arch"
    mkdir "$work/stage/$folder"
    chmod 755 "$work/stage/$folder"
    cp "$bin" "$work/stage/$folder/radio"
    chmod 755 "$work/stage/$folder/radio"
    cp "$work/src/LICENSE" "$work/stage/$folder/LICENSE"
    chmod 644 "$work/stage/$folder/LICENSE"
    COPYFILE_DISABLE=1 tar --uid 0 --gid 0 --uname root --gname wheel --no-mac-metadata --no-xattrs \
        -czf "$work/dist/$folder.tar.gz" -C "$work/stage" "$folder" > /dev/null 2>&1 || fail "could not pack the $target binary"
done
cd "$work/dist"
shasum -a 256 "radio-$tag-macos-arm64.tar.gz" "radio-$tag-macos-x86_64.tar.gz" > SHA256SUMS 2> /dev/null || fail 'could not checksum the archives'
shasum -a 256 -c SHA256SUMS > /dev/null 2>&1 || fail 'archive checksums differ'
out="$root/target/release-dist/$tag"
mkdir -p "$(dirname "$out")"
rm -rf "$out"
mv "$work/dist" "$out"
cd "$root"
if [ "$publish" -eq 0 ]; then
    printf 'release: built %s in %s\n  radio-%s-macos-arm64.tar.gz\n  radio-%s-macos-x86_64.tar.gz\n  SHA256SUMS\n' "$tag" "$out" "$tag" "$tag"
    printf 'release: to publish it, run: bash scripts/release.sh %s --publish\n' "$tag"
    exit 0
fi
remote=$(git -C "$root" ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}" 2> /dev/null) || fail "$tag is not on origin"
remote_commit=$(printf '%s\n' "$remote" | awk -v ref="refs/tags/$tag" '$2 == ref { plain=$1 } $2 == ref "^{}" { peeled=$1 } END { print (peeled != "" ? peeled : plain) }')
[ "$remote_commit" = "$commit" ] || fail "$tag is not on origin"
if gh release view "$tag" --json tagName > /dev/null 2>&1; then
    fail "$tag already has a release"
fi
cat > "$work/notes.md" <<NOTES
The \`radio\` daemon for macOS, built from $tag ($commit).

- \`radio-$tag-macos-arm64.tar.gz\`: Apple silicon
- \`radio-$tag-macos-x86_64.tar.gz\`: Intel
- \`SHA256SUMS\`: check a download with \`shasum -a 256 -c SHA256SUMS --ignore-missing\`

Signed ad hoc, not notarised. Ship it inside your app, whose signature and notarisation cover it; see
\`docs/app-developers.md\`.
NOTES
gh release create "$tag" --verify-tag --title "radio $tag" --notes-file "$work/notes.md" \
    "$out/radio-$tag-macos-arm64.tar.gz" "$out/radio-$tag-macos-x86_64.tar.gz" "$out/SHA256SUMS" > /dev/null 2>&1 || fail 'gh release create failed'
gh release download "$tag" --dir "$work/check" --pattern 'radio-*' --pattern SHA256SUMS > /dev/null 2>&1 || fail "$tag did not read back"
for file in "radio-$tag-macos-arm64.tar.gz" "radio-$tag-macos-x86_64.tar.gz" SHA256SUMS; do
    if [ ! -f "$work/check/$file" ] || ! cmp -s "$out/$file" "$work/check/$file"; then
        fail "$tag did not read back"
    fi
done
printf 'release: published %s (3 files, read back)\n' "$tag"
