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
bash "$root/scripts/release-check.sh" --remote origin "$commit" || exit 1
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

# The short scan reads one partition of the binary. READ: the header and load commands; __TEXT __cstring, __const,
# __ustring and __oslogstring; __DATA and __DATA_CONST __const, __data, __static_data and __thread_data; in __LINKEDIT the
# symbol strings, the bind, weak-bind and lazy-bind streams and the chained fixups. __TEXT,__text is read on its own, in a
# third file where tab, CR and LF break runs: a literal built in place (x86_64 movabs) is text there, a tab beside chance
# letters is not. SKIP, code and tables: every other section of instructions; __eh_frame, __unwind_info and
# __gcc_except_tab; __got, __la_symbol_ptr and __thread_vars; in __LINKEDIT rebase, the export trie (it stores names in
# fragments, so a crate hash can lose its Cs there; every exported name is whole in the symbol strings), function
# starts, data in code, the symbol entries, the indirect symbols and the code signature. A region with file bytes on
# neither list refuses the release: a section of another name, a read name carrying instructions, relocations, a load
# command otool can't name, or a table of one that isn't listed. So does a skipped region over a read one. Bytes between
# regions are read. Adding a region to READ only widens the scan; a move to SKIP, or out of READ, is the owner's call.
# Long terms, paths and the account keep the whole file.
partition() {
    local size verdict where layout_failure="could not read the $target load commands"
    otool -l "$1" > "$work/load-commands" 2> /dev/null || fail "$layout_failure"
    size=$(wc -c < "$1") || fail "$layout_failure"
    size=${size//[[:space:]]/}
    awk -v size="$size" '
        function hex(s,   v, i) {
            s = tolower(s)
            sub(/^0x/, "", s)
            v = 0
            for (i = 1; i <= length(s); i++) v = v * 16 + index("0123456789abcdef", substr(s, i, 1)) - 1
            return v
        }
        # kind: trie and code fail with their own messages; other with the layout one.
        function region(list, kind, off, len) {
            seen[kind] = 1
            if (lc == "" || off !~ /^[0-9]+$/ || len !~ /^[0-9]+$/) { bad[kind] = 1; return }
            if (len + 0 == 0) return
            if (list == "unlisted") { if (!has_unlisted) unlisted = lc; has_unlisted = 1; return }
            if (off + len > size + 0) { bad[kind] = 1; return }
            n++
            lists[n] = list; offs[n] = off + 0; lens[n] = len + 0; lcs[n] = lc
        }
        function flush(   kind, f, type, name, list, instructions) {
            if (!insect) return
            insect = 0
            kind = (seg == "__TEXT" && sect == "__text") ? "code" : "other"
            if (seg == "" || sect == "" || sz !~ /^0x[0-9a-fA-F]+$/ || fl !~ /^0x[0-9a-fA-F]+$/ || nreloc !~ /^[0-9]+$/) {
                bad[kind] = 1
                return
            }
            region("unlisted", kind, 0, nreloc)
            f = hex(fl)
            type = f % 256
            # Zero-fill (S_ZEROFILL, S_GB_ZEROFILL, S_THREAD_LOCAL_ZEROFILL) has no file bytes.
            if (type == 1 || type == 12 || type == 18) return
            name = seg "," sect
            # S_ATTR_PURE_INSTRUCTIONS or S_ATTR_SOME_INSTRUCTIONS.
            instructions = int(f / 2147483648) % 2 || int(f / 1024) % 2
            if (kind == "code") list = "code"
            else if (name ~ /^__TEXT,__(cstring|const|ustring|oslogstring)$/) list = instructions ? "unlisted" : "read"
            else if (name ~ /^__DATA(_CONST)?,__(const|data|static_data|thread_data)$/) list = instructions ? "unlisted" : "read"
            else if (instructions) list = "skip"
            else if (name ~ /^__TEXT,__(eh_frame|unwind_info|gcc_except_tab)$/) list = "skip"
            else if (name ~ /^__DATA(_CONST)?,__(got|la_symbol_ptr|thread_vars)$/) list = "skip"
            else list = "unlisted"
            region(list, kind, soff, hex(sz))
        }
        function count(n, width) { return n ~ /^[0-9]+$/ ? n * width : n }
        $1 == "Load" && $2 == "command" {
            flush()
            lc = $3
            if (lc !~ /^[0-9]+$/) bad["other"] = 1
            cmd = ""
            delete val
            next
        }
        $1 == "cmd" { cmd = $2; if (cmd !~ /^LC_[A-Z0-9_]+$/) region("unlisted", "other", 0, 1); next }
        $1 == "cmdsize" { if ($2 ~ /^[0-9]+$/) cmds += $2; else bad["other"] = 1; next }
        $1 == "Section" { flush(); insect = 1; sect = ""; seg = ""; sz = ""; soff = ""; fl = ""; nreloc = ""; next }
        insect {
            if ($1 == "sectname") sect = $2
            else if ($1 == "segname") seg = $2
            else if ($1 == "size") sz = $2
            else if ($1 == "offset") soff = $2
            else if ($1 == "flags") fl = $2
            else if ($1 == "nreloc") nreloc = $2
            next
        }
        { val[$1] = $2 }
        cmd ~ /^LC_DYLD_INFO(_ONLY)?$/ && $1 ~ /_size$/ {
            base = substr($1, 1, length($1) - 5)
            if (base == "rebase") region("skip", "other", val["rebase_off"], $2)
            else if (base == "export") region("skip", "trie", val["export_off"], $2)
            else if (base ~ /^(bind|weak_bind|lazy_bind)$/) region("read", "other", val[base "_off"], $2)
            else region("unlisted", "other", val[base "_off"], $2)
        }
        cmd == "LC_SYMTAB" && $1 == "nsyms" { region("skip", "other", val["symoff"], count($2, 16)) }
        cmd == "LC_SYMTAB" && $1 == "strsize" { region("read", "other", val["stroff"], $2) }
        cmd == "LC_DYSYMTAB" && $1 == "nindirectsyms" { region("skip", "other", val["indirectsymoff"], count($2, 4)) }
        cmd == "LC_DYSYMTAB" && $1 ~ /^(ntoc|nmodtab|nextrefsyms|nextrel|nlocrel)$/ { region("unlisted", "other", 0, $2) }
        cmd ~ /^LC_(NOTE|SYMSEG)$/ && $1 == "size" { region("unlisted", "other", 0, $2) }
        cmd == "LC_TWOLEVEL_HINTS" && $1 == "nhints" { region("unlisted", "other", 0, $2) }
        cmd ~ /^LC_ENCRYPTION_INFO(_64)?$/ && $1 == "cryptsize" { region("unlisted", "other", 0, $2) }
        $1 == "datasize" {
            if (cmd == "LC_DYLD_EXPORTS_TRIE") region("skip", "trie", val["dataoff"], $2)
            else if (cmd ~ /^LC_(FUNCTION_STARTS|DATA_IN_CODE|CODE_SIGNATURE)$/) region("skip", "other", val["dataoff"], $2)
            else if (cmd == "LC_DYLD_CHAINED_FIXUPS") region("read", "other", val["dataoff"], $2)
            else region("unlisted", "other", val["dataoff"], $2)
        }
        END {
            flush()
            lc = "header"
            region("read", "other", 0, 32 + cmds)
            if (bad["trie"] || !seen["trie"]) { print "fail trie"; exit }
            if (bad["code"] || !seen["code"]) { print "fail code"; exit }
            if (bad["other"]) { print "fail layout"; exit }
            if (has_unlisted) { print "fail unlisted", unlisted; exit }
            for (i = 1; i <= n; i++) {
                if (lists[i] == "read") continue
                for (j = 1; j <= n; j++) {
                    if (lists[j] == "read" && offs[i] < offs[j] + lens[j] && offs[j] < offs[i] + lens[i]) {
                        print "fail overlap", lcs[i]
                        exit
                    }
                }
            }
            for (i = 1; i <= n; i++) print lists[i], offs[i], lens[i]
            print "ok"
        }
    ' "$work/load-commands" > "$work/regions" 2> /dev/null || fail "$layout_failure"
    verdict=$(tail -n 1 "$work/regions") || fail "$layout_failure"
    where=${verdict##* }
    case "$verdict" in
        ok) ;;
        'fail trie') fail "could not read the $target export trie" ;;
        'fail code') fail "could not read the $target machine code" ;;
        'fail unlisted '*)
            [[ "$where" =~ ^[0-9]+$ ]] || fail "$layout_failure"
            fail "the $target binary has a region the short scan does not list (load command $where)" ;;
        'fail overlap '*)
            [[ "$where" =~ ^[0-9]+$ ]] || fail "$layout_failure"
            fail "the $target binary has a skipped region over a read one (load command $where)" ;;
        *) fail "$layout_failure" ;;
    esac
    # The copy keeps every read byte and zeroes the rest; the code file holds __TEXT,__text alone.
    perl -e '
        my ($bin, $regions, $copy, $code) = @ARGV;
        open(my $in, "<:raw", $bin) or die;
        my $data = do { local $/; <$in> };
        close($in) or die;
        my ($text, $machine) = ($data, "");
        open(my $list, "<", $regions) or die;
        while (my $line = <$list>) {
            my ($kind, $off, $len) = split " ", $line;
            last if $kind eq "ok";
            die unless $kind =~ /^(read|code|skip)$/ && $off =~ /^\d+$/ && $len =~ /^\d+$/;
            next if $kind eq "read";
            $machine .= substr($data, $off, $len) if $kind eq "code";
            substr($text, $off, $len, "\0" x $len);
        }
        for ([$copy, $text], [$code, $machine]) {
            open(my $out, ">:raw", $_->[0]) or die;
            print {$out} $_->[1] or die;
            close($out) or die;
        }
    ' "$1" "$work/regions" "$2" "$3" 2> /dev/null || fail "$layout_failure"
    [ "$(wc -c < "$2" | tr -d '[:space:]')" = "$size" ] || fail "$layout_failure"
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
    # The copy's printable runs, tab, CR and LF counted as text; not strings -a, which skips Mach-O load commands and
    # __LINKEDIT. No runs fails closed.
    partition "$bin" "$work/text-source" "$work/code"
    tr '\t\r\n' '   ' < "$work/text-source" | grep -a -o -E '[[:print:]]{4,}' > "$work/strings" 2> /dev/null || fail "could not scan the $target binary"
    # Then the machine code's own runs, where tab, CR and LF break them; real code always has some, so none fails closed.
    grep -a -o -E '[[:print:]]{4,}' "$work/code" >> "$work/strings" 2> /dev/null || fail "could not read the $target machine code"
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
bash "$root/scripts/release-check.sh" --remote origin "$commit" || exit 1
gh release edit "$tag" -R "$repo" --draft=false > /dev/null 2>&1 || fail "could not publish the $tag draft"
is_draft=$(gh release view "$tag" -R "$repo" --json isDraft --jq .isDraft 2> /dev/null) || fail "$tag is still a draft"
[ "$is_draft" = false ] || fail "$tag is still a draft"
printf 'release: published %s to %s (3 files, read back)\n' "$tag" "$repo"
