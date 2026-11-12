"""Generate the full casefold exceptions to Rust's per-character lowercase."""
import sys
import unicodedata


def rust_string(text):
    return ''.join('\\' + c if c in '\\"' else c if ' ' <= c <= '~'
                   else f'\\u{{{ord(c):04x}}}' for c in text)


print(f'// Generated with Python {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}, Unicode {unicodedata.unidata_version}.')
print('// python3 crates/radio-panels/gen_casefold.py > crates/radio-panels/src/casefold_table.rs')
print('pub(crate) static CASEFOLD: &[(char, &str)] = &[')
for code in range(0x110000):
    if 0xD800 <= code <= 0xDFFF:
        continue
    char = chr(code)
    if char.casefold() != char.lower():
        print(f'    (\'\\u{{{code:04x}}}\', "{rust_string(char.casefold())}"),')
print('];')
