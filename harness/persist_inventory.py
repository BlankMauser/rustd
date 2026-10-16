#!/usr/bin/env python3
"""Counts the Salsa ingredients each crate declares and lists what every query returns.

Usage: harness/persist_inventory.py [--queries]

An ingredient is an input, an interned struct, a tracked struct or a tracked function; each one
has to opt in with `persist` before Salsa's `persistence` feature serializes it. With `--queries`
every tracked function is printed with its return type, which is what needs serde support.
"""
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "crates"
ATTRIBUTE = re.compile(r"#\[salsa(?:_macros)?::(input|interned|tracked)\b")
ITEM = re.compile(r"\b(struct|fn|impl|trait)\b")
INTERN_MACRO = re.compile(r"^\s*(impl_intern|impl_intern_key|impl_wrapper)!\(", re.M)


def balanced(text, start, open_ch, close_ch):
    depth = 0
    for i in range(start, len(text)):
        if text[i] == open_ch:
            depth += 1
        elif text[i] == close_ch:
            depth -= 1
            if depth == 0:
                return i + 1
    return len(text)


def attribute_end(text, start):
    return balanced(text, text.index("[", start), "[", "]")


def skip_attributes(text, pos):
    while True:
        rest = text[pos:]
        stripped = rest.lstrip()
        pos += len(rest) - len(stripped)
        if stripped.startswith("#["):
            pos = attribute_end(text, pos)
        elif stripped.startswith("//"):
            pos = text.index("\n", pos)
        else:
            return pos


def return_type(text, fn_pos):
    params_end = balanced(text, text.index("(", fn_pos), "(", ")")
    tail = re.match(r"\s*->\s*(.*?)\s*(?:\bwhere\b|\{|;)", text[params_end:], re.S)
    return " ".join(tail.group(1).split()) if tail else "()"


def scan(path, counts, queries):
    text = path.read_text()
    crate = path.relative_to(ROOT).parts[0]
    if "macro_rules! impl_intern_key" not in text:
        counts[crate]["interned"] += len(INTERN_MACRO.findall(text))
    for found in ATTRIBUTE.finditer(text):
        line_start = text.rfind("\n", 0, found.start()) + 1
        if text[line_start:found.start()].lstrip().startswith("//"):
            continue
        kind = found.group(1)
        if kind != "tracked":
            counts[crate][kind] += 1
            continue
        item = ITEM.search(text, skip_attributes(text, attribute_end(text, found.start())))
        if item is None or item.group(1) in ("impl", "trait"):
            continue
        if item.group(1) == "struct":
            counts[crate]["tracked struct"] += 1
            continue
        counts[crate]["tracked fn"] += 1
        name = re.match(r"fn\s+(\w+)", text[item.start():]).group(1)
        attribute = " ".join(text[found.start():attribute_end(text, found.start())].split())
        queries[crate].append((name, return_type(text, item.start()), attribute, path))


def main():
    counts = defaultdict(Counter)
    queries = defaultdict(list)
    for path in sorted(ROOT.rglob("*.rs")):
        if "/tests/" in str(path) or path.name == "tests.rs" or "test-fixture" in path.parts:
            continue
        scan(path, counts, queries)
    kinds = ["input", "interned", "tracked struct", "tracked fn"]
    print(f"{'crate':<14}" + "".join(f"{kind:>16}" for kind in kinds))
    total = Counter()
    for crate in sorted(c for c in counts if sum(counts[c].values())):
        total.update(counts[crate])
        print(f"{crate:<14}" + "".join(f"{counts[crate][kind]:>16}" for kind in kinds))
    print(f"{'total':<14}" + "".join(f"{total[kind]:>16}" for kind in kinds))
    if "--queries" in sys.argv:
        for crate in sorted(queries):
            for name, returns, attribute, path in queries[crate]:
                print(f"{crate}\t{name}\t{returns}\t{attribute}\t{path.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
