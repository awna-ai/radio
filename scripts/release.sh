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
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY
unset GIT_ALTERNATE_OBJECT_DIRECTORIES GH_REPO GH_HOST RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
while IFS= read -r variable; do
    case "$variable" in CARGO_PROFILE_*|CARGO_BUILD_*|CARGO_TARGET_*) unset "$variable" ;; esac
done < <(compgen -e)
root=$(cd "$(dirname "$0")/.." && pwd -P)
fail() {
    printf 'release: %s\n' "$1" >&2
    exit 1
}
commit=$(git -C "$root" rev-parse --verify -q "refs/tags/$tag^{commit}") || fail "no tag $tag"
terms=${RADIO_TERMS_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/radio-dev/terms.txt}
[ -f "$terms" ] && [ -r "$terms" ] || fail "no term list at $terms"

# Keep the list private and remove every temporary file, including on a refusal.
temp_dir=${TMPDIR:-/tmp}
temp_dir=${temp_dir%/}
list=$(mktemp "$temp_dir/radio-release.XXXXXX")
work=''
long=''
short=''
gh_error=''
cleanup() {
    rm -f "$list" "$long" "$short" "$gh_error"
    if [ -n "$work" ]; then
        git -C "$root" worktree remove --force "$work/src" > /dev/null 2>&1 || true
        git -C "$root" worktree prune > /dev/null 2>&1 || true
        rm -rf "$work"
    fi
}
trap cleanup EXIT
long=$(mktemp "$temp_dir/radio-release.XXXXXX")
short=$(mktemp "$temp_dir/radio-release.XXXXXX")
sed -e 's/\r$//' -e 's/[[:space:]]*$//' -e 's/^[[:space:]]*//' -e '/^[[:space:]]*#/d' -e '/^$/d' "$terms" > "$list" || fail "no term list at $terms"
[ -s "$list" ] || fail "no term list at $terms"
awk 'length($0) >= 5' "$list" > "$long" || fail 'could not split the term list'
awk 'length($0) < 5' "$list" > "$short" || fail 'could not split the term list'

tools=(cargo rustup lipo strip codesign shasum tar otool perl)
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
# Resolve annotated tags on the same repository that receives the release.
tag_on_github() {
    local object kind sha depth=0 endpoint="repos/$repo/git/ref/tags/$tag"
    local diagnostic="$tag is not on $repo at $commit${1:-}"
    while :; do
        gh api "$endpoint" --jq '.object.type + " " + .object.sha' > "$gh_error" 2>&1 || fail "$diagnostic"
        object=$(cat "$gh_error") || fail "$diagnostic"
        kind=${object%% *}
        sha=${object#* }
        case "$kind" in
            commit)
                [ "$sha" = "$commit" ] || fail "$diagnostic"
                return ;;
            tag)
                [ "$depth" -lt 4 ] || fail "$diagnostic"
                depth=$((depth + 1))
                endpoint="repos/$repo/git/tags/$sha"
                ;;
            *) fail "$diagnostic" ;;
        esac
    done
}
if [ "$publish" -eq 1 ]; then
    url=$(git -C "$root" config --get remote.origin.url) || fail 'origin is not a GitHub repository'
    case "$url" in
        git@github.com:*) repo=${url#git@github.com:} ;;
        ssh://git@github.com/*) repo=${url#ssh://git@github.com/} ;;
        https://github.com/*) repo=${url#https://github.com/} ;;
        *) fail 'origin is not a GitHub repository' ;;
    esac
    repo=${repo%.git}
    [[ "$repo" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || fail 'origin is not a GitHub repository'
    gh_error=$(mktemp "$temp_dir/radio-release.XXXXXX")
    tag_on_github
    if gh release view "$tag" -R "$repo" --json tagName > /dev/null 2> "$gh_error"; then
        fail "$tag already has a release (a draft counts); remove it with: gh release delete $tag -R $repo --yes"
    else
        status=$?
        if [ "$status" -ne 1 ] || ! grep -F -q 'release not found' "$gh_error"; then
            fail "could not ask GitHub about $tag"
        fi
    fi
fi
work=$(mktemp -d "$temp_dir/radio-release.XXXXXX")
work=$(cd "$work" && pwd -P)
git -C "$root" worktree add -q --detach "$work/src" "$commit" 2> /dev/null || fail 'could not copy the tag'
cd "$work/src"
id=$(cargo pkgid -p radio 2> /dev/null) || fail 'could not read the workspace version'
version=${id##*[#@]}
[ "v$version" = "$tag" ] || fail "$tag is not the workspace version $version"
unset RUSTFLAGS CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_RUSTFLAGS
CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/home"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"$'\x1f'"--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"$'\x1f'"--remap-path-prefix=$work=/build"
export CARGO_ENCODED_RUSTFLAGS
# A v0 crate hash is dropped only before a crate of this build; a crate missing here keeps its hash, so the scan refuses.
crate_failure='could not read the crates of this build; fetch them with: cargo fetch --locked'
cargo metadata --locked --offline --format-version 1 --filter-platform "${targets[0]}" --filter-platform "${targets[1]}" > "$work/metadata" 2> /dev/null || fail "$crate_failure"
grep -o -E '"kind":\[[^]]*"(lib|rlib|dylib|bin)"[^]]*\],"crate_types":\[[^]]*\],"name":"[^"]+"' "$work/metadata" > "$work/targets" 2> /dev/null || fail "$crate_failure"
{
    sed -E 's/.*"name":"([^"]+)"$/\1/' "$work/targets" | tr - _
    printf '%s\n' std core alloc compiler_builtins panic_unwind panic_abort unwind std_detect rustc_demangle hashbrown \
        addr2line gimli object miniz_oxide adler2 memchr cfg_if libc rustc_std_workspace_core rustc_std_workspace_alloc __rustc
} | sort -u > "$work/crates" 2> /dev/null || fail "$crate_failure"
grep -F -x -q radio "$work/crates" 2> /dev/null || fail "$crate_failure"
if grep -v -E -q '^[A-Za-z_][A-Za-z0-9_]*$' "$work/crates" 2> /dev/null; then
    fail "$crate_failure"
fi
# v0 writes a separator _ between the length and a name that starts with _.
crate_names=$(awk '{ printf "%s%d%s%s", (NR > 1 ? "|" : ""), length($0), (substr($0, 1, 1) == "_" ? "_" : ""), $0 }' "$work/crates") || fail "$crate_failure"

# The export trie stores names in fragments, so a crate hash can lose its Cs there; every exported name is whole in the
# symbol table, which the short scan still reads. Long terms, paths and the account keep the whole file.
without_trie() {
    local failure="could not read the $target export trie" size off length end
    otool -l "$1" > "$work/load-commands" 2> /dev/null || fail "$failure"
    awk '
        $1 == "cmd" { cmd = $2; off = "" }
        cmd == "LC_DYLD_INFO_ONLY" && $1 == "export_off" { off = $2 }
        cmd == "LC_DYLD_INFO_ONLY" && $1 == "export_size" { print off, $2 }
        cmd == "LC_DYLD_EXPORTS_TRIE" && $1 == "dataoff" { off = $2 }
        cmd == "LC_DYLD_EXPORTS_TRIE" && $1 == "datasize" { print off, $2 }
    ' "$work/load-commands" > "$work/trie" 2> /dev/null || fail "$failure"
    [ -s "$work/trie" ] || fail "$failure"
    size=$(wc -c < "$1") || fail "$failure"
    size=${size//[[:space:]]/}
    cp "$1" "$2" || fail "$failure"
    while read -r off length; do
        [[ "$off" =~ ^[0-9]+$ && "$length" =~ ^[0-9]+$ ]] || fail "$failure"
        end=$((off + length))
        [ "$length" -gt 0 ] || continue
        { head -c "$off" "$2" && head -c "$length" /dev/zero && tail -c "+$((end + 1))" "$2"; } > "$2.next" 2> /dev/null || fail "$failure"
        mv "$2.next" "$2" || fail "$failure"
    done < "$work/trie"
    [ "$(wc -c < "$2" | tr -d '[:space:]')" = "$size" ] || fail "$failure"
}

normalize() {
    sed -E 's/_/ /g; s/([a-z0-9])([A-Z])/\1 \2/g; s/([A-Z])([A-Z][a-z])/\1 \2/g; s/([A-Za-z])([0-9])/\1 \2/g; s/([0-9])([A-Za-z])/\1 \2/g' "$@"
}

# A failed grep is distinct from a clean scan; never print the matched bytes.
check_terms() {
    local bytes=$1 text=$2 file status words
    local flags
    for words in "$long" "$short"; do
        [ -s "$words" ] || continue
        flags=(-a -F -i)
        file=$bytes
        if [ "$words" = "$short" ]; then
            flags+=(-w)
            file=$text
        fi
        if grep "${flags[@]}" -f "$words" -- "$file" > /dev/null 2>&1; then
            fail "the $target binary names a private term"
        else
            status=$?
            [ "$status" -eq 1 ] || fail "could not scan the $target binary"
        fi
    done
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
    if grep -a -F -i -q -- "$account" "$bin" > /dev/null 2>&1; then
        fail "the $target binary names this account"
    else
        status=$?
        [ "$status" -eq 1 ] || fail "could not scan the $target binary"
    fi
    # Whole-file printable runs but the export trie, tab, CR and LF counted as text: strings -a skips Mach-O load commands and
    # __LINKEDIT; no runs fails closed.
    without_trie "$bin" "$work/text-source"
    tr '\t\r\n' '   ' < "$work/text-source" | grep -a -o -E '[[:print:]]{4,}' > "$work/strings" 2> /dev/null || fail "could not scan the $target binary"
    check_terms "$bin" "$work/strings"
    normalize "$bin" > "$work/normalized" 2> /dev/null || fail "could not scan the $target binary"
    # v0 crate hashes (Cs, 1 to 11 base-62 digits, _ before a crate of this build) are build noise: drop them before splitting words.
    # perl, since a real build's crate list is longer than macOS sed takes in one command.
    CRATE_NAMES=$crate_names perl -pe 's/Cs[0-9A-Za-z]{1,11}_(u?)($ENV{CRATE_NAMES})/ $1$2/g' < "$work/strings" 2> /dev/null | normalize > "$work/strings-normalized" 2> /dev/null || fail "could not scan the $target binary"
    check_terms "$work/normalized" "$work/strings-normalized"
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
mkdir -p "$out"
cp -p "$work/dist/"* "$out/"
cd "$root"
if [ "$publish" -eq 0 ]; then
    printf 'release: built %s in %s\n  radio-%s-macos-arm64.tar.gz\n  radio-%s-macos-x86_64.tar.gz\n  SHA256SUMS\n' "$tag" "$out" "$tag" "$tag"
    printf 'release: to publish it, run: bash scripts/release.sh %s --publish\n' "$tag"
    exit 0
fi
cat > "$work/notes.md" <<NOTES
The \`radio\` daemon for macOS, built from $tag ($commit).

- \`radio-$tag-macos-arm64.tar.gz\`: Apple silicon
- \`radio-$tag-macos-x86_64.tar.gz\`: Intel
- \`SHA256SUMS\`: check a download with \`shasum -a 256 -c SHA256SUMS --ignore-missing\`

Signed ad hoc, not notarised. Ship it inside your app, whose signature and notarisation cover it; see
*For app developers* in the README.
NOTES
(cd "$work/dist" && shasum -a 256 -c SHA256SUMS) > /dev/null 2>&1 || fail "$tag changed before upload"
gh release create "$tag" -R "$repo" --draft --verify-tag --title "radio $tag" --notes-file "$work/notes.md" \
    "$work/dist/radio-$tag-macos-arm64.tar.gz" "$work/dist/radio-$tag-macos-x86_64.tar.gz" "$work/dist/SHA256SUMS" > /dev/null 2>&1 || fail 'gh release create failed'
readback_failure="$tag did not read back; the draft is still there: gh release delete $tag -R $repo --yes"
gh release download "$tag" -R "$repo" --dir "$work/check" --pattern 'radio-*' --pattern SHA256SUMS > /dev/null 2>&1 || fail "$readback_failure"
printf '%s\n' "radio-$tag-macos-arm64.tar.gz" "radio-$tag-macos-x86_64.tar.gz" SHA256SUMS | sort > "$work/expected"
(cd "$work/check" && find . -mindepth 1 -maxdepth 1 -print | sed 's|^./||' | sort) > "$work/actual" || fail "$readback_failure"
cmp -s "$work/expected" "$work/actual" || fail "$readback_failure"
for file in "radio-$tag-macos-arm64.tar.gz" "radio-$tag-macos-x86_64.tar.gz" SHA256SUMS; do
    if [ ! -f "$work/check/$file" ] || ! cmp -s "$work/dist/$file" "$work/check/$file"; then
        fail "$readback_failure"
    fi
done
gh release view "$tag" -R "$repo" --json assets --jq '.assets[].name' > "$work/assets" 2> /dev/null || fail "$readback_failure"
sort "$work/assets" > "$work/actual" || fail "$readback_failure"
cmp -s "$work/expected" "$work/actual" || fail "$readback_failure"
tag_on_github "; the draft is still there: gh release delete $tag -R $repo --yes"
gh release edit "$tag" -R "$repo" --draft=false > /dev/null 2>&1 || fail "could not publish the $tag draft"
is_draft=$(gh release view "$tag" -R "$repo" --json isDraft --jq .isDraft 2> /dev/null) || fail "$tag is still a draft"
[ "$is_draft" = false ] || fail "$tag is still a draft"
printf 'release: published %s to %s (3 files, read back)\n' "$tag" "$repo"
