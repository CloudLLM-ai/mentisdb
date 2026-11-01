"""Rewrite GFM table separator rows so pandoc's LaTeX column widths follow content.

Pandoc derives relative column widths from the dashes in a pipe table's
separator row once a row is wider than the page. Markdown authors write
`|---|---|`, which gives every column the same width. This filter sets each
column's dash count to its longest cell (between a floor and a cap), so a
narrow "Version" column no longer takes as much room as a sentence.

Usage: python3 -I table_widths.py < in.md > out.md
"""
import re
import sys

SEP = re.compile(r"^\|(\s*:?-+:?\s*\|)+\s*$")


CODE = re.compile(r"`([^`]*)`")


def width(cell):
    """Approximate rendered width: monospace code is about 1.3x as wide as text."""
    code = sum(len(m) for m in CODE.findall(cell))
    text = len(CODE.sub("", cell))
    return int(text + 1.3 * code + 0.5)


def cells(row):
    return [c.strip() for c in row.strip().strip("|").split("|")]


def flush(block, out):
    if len(block) >= 2 and SEP.match(block[1]):
        rows = [cells(r) for i, r in enumerate(block) if i != 1]
        n = len(cells(block[1]))
        widths = [max(8, min(60, max((width(r[i]) if i < len(r) else 0) for r in rows))) for i in range(n)]
        if sum(widths) + n + 1 > 72:
            block[1] = "|" + "|".join("-" * w for w in widths) + "|"
    out.extend(block)


def main():
    out, block = [], []
    for line in sys.stdin.read().split("\n"):
        if line.startswith("|"):
            block.append(line)
            continue
        flush(block, out)
        block = []
        out.append(line)
    flush(block, out)
    sys.stdout.write("\n".join(out))


if __name__ == "__main__":
    main()
