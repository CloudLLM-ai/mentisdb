#!/usr/bin/env bash
# Rebuild WHITEPAPER.pdf from WHITEPAPER.tex.
# Usage: ./build-whitepaper.sh [--open] [--clean]
#
# -----------------------------------------------------------------------------
# Examples
# -----------------------------------------------------------------------------
#
#     ./build-whitepaper.sh           # rebuild WHITEPAPER.pdf
#     ./build-whitepaper.sh --open    # rebuild and open the PDF
#     ./build-whitepaper.sh --clean   # remove aux files (.aux/.log/...) and exit
#     ./build-whitepaper.sh --help    # show short help
#
# Typical workflow after editing WHITEPAPER.tex:
#
#     vim WHITEPAPER.tex
#     ./build-whitepaper.sh --open
#
# The script runs pdflatex twice (first pass writes .aux, second pass resolves
# cross-references and the bibliography), then removes intermediate files so
# only WHITEPAPER.pdf remains. Exit status is non-zero on any LaTeX error.
#
# -----------------------------------------------------------------------------
# TeX toolchain install
# -----------------------------------------------------------------------------
#
# macOS (Homebrew) — BasicTeX is ~100 MB; MacTeX is ~5 GB (full distribution):
#
#     brew install --cask basictex
#     # open a new shell, or source the PATH helper:
#     eval "$(/usr/libexec/path_helper -s)"
#     sudo tlmgr update --self
#     # install the extra packages this document uses but BasicTeX omits:
#     sudo tlmgr install booktabs mathtools enumitem microtype lm
#
#     # Alternative (full distribution, no extra tlmgr calls needed):
#     # brew install --cask mactex-no-gui
#
# Ubuntu / Debian — texlive-latex-extra covers everything used here:
#
#     sudo apt update
#     sudo apt install -y texlive-latex-base \
#                         texlive-latex-recommended \
#                         texlive-latex-extra \
#                         texlive-fonts-recommended \
#                         texlive-science
#
#     # Or the kitchen sink (bigger, simpler):
#     # sudo apt install -y texlive-full
#
# Verify with: pdflatex --version
# -----------------------------------------------------------------------------

set -euo pipefail

cd "$(dirname "$0")"

TEX_FILE="WHITEPAPER.tex"
PDF_FILE="WHITEPAPER.pdf"
OPEN_AFTER=0
CLEAN_ONLY=0

for arg in "$@"; do
  case "$arg" in
    --open) OPEN_AFTER=1 ;;
    --clean) CLEAN_ONLY=1 ;;
    -h|--help)
      echo "Usage: $0 [--open] [--clean]"
      echo "  --open   Open the PDF when the build finishes"
      echo "  --clean  Remove aux files and exit"
      exit 0
      ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

cleanup_aux() {
  rm -f WHITEPAPER.aux WHITEPAPER.log WHITEPAPER.out \
        WHITEPAPER.toc WHITEPAPER.fls WHITEPAPER.fdb_latexmk \
        WHITEPAPER.synctex.gz
}

if [[ $CLEAN_ONLY -eq 1 ]]; then
  cleanup_aux
  echo "Cleaned aux files."
  exit 0
fi

if [[ ! -f "$TEX_FILE" ]]; then
  echo "Error: $TEX_FILE not found in $(pwd)" >&2
  exit 1
fi

if ! command -v pdflatex >/dev/null 2>&1; then
  if [[ -x /usr/libexec/path_helper ]]; then
    eval "$(/usr/libexec/path_helper -s)"
  fi
fi

if ! command -v pdflatex >/dev/null 2>&1; then
  echo "Error: pdflatex not found. Install a TeX distribution:" >&2
  echo "  brew install --cask basictex   # ~100 MB" >&2
  echo "  brew install --cask mactex-no-gui   # ~5 GB (full)" >&2
  echo "Then open a new shell, or run:" >&2
  echo "  eval \"\$(/usr/libexec/path_helper -s)\"" >&2
  exit 127
fi

echo "==> pdflatex pass 1/2"
pdflatex -interaction=nonstopmode -halt-on-error "$TEX_FILE" >/dev/null

echo "==> pdflatex pass 2/2 (resolves cross-references)"
pdflatex -interaction=nonstopmode -halt-on-error "$TEX_FILE" >/dev/null

cleanup_aux

if [[ ! -f "$PDF_FILE" ]]; then
  echo "Error: build finished but $PDF_FILE was not produced." >&2
  exit 1
fi

SIZE=$(wc -c <"$PDF_FILE" | tr -d ' ')
echo "==> Built $PDF_FILE (${SIZE} bytes)"

if [[ $OPEN_AFTER -eq 1 ]]; then
  open "$PDF_FILE"
fi
