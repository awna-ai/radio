"""Generate Python 3.12's word and simple lowercase tables for the leak guard."""
import _sre
import sys
import unicodedata


def main():
    if sys.version_info[:2] != (3, 12) or unicodedata.unidata_version != "15.0.0":
        print("Python 3.12 (Unicode 15.0.0) is required; run with python3.12", file=sys.stderr)
        return 1

    word = []
    lower = []
    for code in range(0x110000):
        if 0xD800 <= code <= 0xDFFF:
            continue
        char = chr(code)
        if char.isalnum() or char == "_":
            if word and word[-1][1] + 1 == code:
                word[-1] = (word[-1][0], code)
            else:
                word.append((code, code))
        folded = _sre.unicode_tolower(code)
        if folded != code:
            lower.append((code, folded))

    version = ".".join(str(n) for n in sys.version_info[:3])
    print(f"// Generated with Python {version}, Unicode {unicodedata.unidata_version}.")
    print("// python3.12 crates/radio-panels/gen_sre_tables.py > crates/radio-panels/src/sre_table.rs")
    for name, rows in (("WORD", word), ("LOWER", lower)):
        print(f"pub(crate) static {name}: &[(char, char)] = &[")
        for first, last in rows:
            print(f"    ('\\u{{{first:04x}}}', '\\u{{{last:04x}}}'),")
        print("];")
    return 0


if __name__ == "__main__":
    sys.exit(main())
