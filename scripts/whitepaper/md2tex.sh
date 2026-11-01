#!/usr/bin/env bash
# Regenerate WHITEPAPER.tex from WHITEPAPER.md so the two never drift apart.
#
# WHITEPAPER.md is the source of truth. Edit it, then run:
#
#     scripts/whitepaper/md2tex.sh      # writes WHITEPAPER.tex
#     ./build-whitepaper.sh             # builds WHITEPAPER.pdf from it
#
# or both at once with `./build-whitepaper.sh --from-md`.
#
# The Markdown file must start with this block, which becomes the LaTeX title,
# author, date and abstract instead of body text:
#
#     # <title>
#     **Author:** <name>
#     **Version:** <version text>
#     **Date:** <YYYY-MM-DD>
#     ---
#     ## Abstract
#     <paragraphs>
#     ---
#
# Requires pandoc 3 (brew install pandoc / apt install pandoc). The generated
# LaTeX uses `listings` for code (no `framed` dependency) and the preamble in
# scripts/whitepaper/header.tex; it builds with a stock BasicTeX install.

set -euo pipefail

cd "$(dirname "$0")/../.."

MD="WHITEPAPER.md"
TEX="WHITEPAPER.tex"
HEADER="scripts/whitepaper/header.tex"

command -v pandoc >/dev/null 2>&1 || {
  echo "Error: pandoc not found (brew install pandoc)" >&2
  exit 127
}

field() { sed -n "s/^\*\*$1:\*\* *//p" "$MD" | head -1; }

TITLE=$(sed -n '1s/^# *//p' "$MD")
AUTHOR=$(field Author)
VERSION=$(field Version)
DATE=$(field Date)
[[ -n "$TITLE" && -n "$AUTHOR" && -n "$DATE" ]] || {
  echo "Error: $MD does not start with the title/author/date block" >&2
  exit 1
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Abstract: the text between "## Abstract" and the next "---" line.
awk '/^## Abstract$/{on=1; next} on && /^---$/{exit} on{print}' "$MD" >"$TMP/abstract.md"
# Body: everything after the second "---" line (the one closing the abstract).
awk 'n>=2{print} /^---$/{n++}' "$MD" | python3 -I scripts/whitepaper/table_widths.py >"$TMP/body.md"

pandoc "$TMP/abstract.md" -f markdown -t latex -o "$TMP/abstract.tex"

pandoc "$TMP/body.md" \
  -f markdown-auto_identifiers+gfm_auto_identifiers \
  -t latex --standalone --syntax-highlighting=idiomatic \
  --include-in-header="$HEADER" \
  -V documentclass=article -V classoption=11pt -V classoption=a4paper \
  -V fontfamily=lmodern -V colorlinks=true -V linkcolor=black -V urlcolor=blue \
  -V "title=\\textbf{${TITLE%%:*}}\\\\[0.3em]\\large ${TITLE#*: }" \
  -V "author=${AUTHOR}" \
  -V "date=${VERSION} --- ${DATE}" \
  -o "$TMP/out.tex"

# Insert the abstract right after \maketitle and drop horizontal rules, which
# only separate sections in Markdown.
awk -v abs="$TMP/abstract.tex" '
  /\\maketitle/ {
    print; print "\\begin{abstract}";
    while ((getline line < abs) > 0) print line;
    print "\\end{abstract}"; next
  }
  /^\\begin\{center\}\\rule\{0\.5\\linewidth\}\{0\.5pt\}\\end\{center\}$/ { next }
  { print }
' "$TMP/out.tex" >"$TEX"

{
  echo "% GENERATED from WHITEPAPER.md by scripts/whitepaper/md2tex.sh. Do not edit by hand."
  cat "$TEX"
} >"$TMP/final.tex"
mv "$TMP/final.tex" "$TEX"

echo "==> Wrote $TEX from $MD"
