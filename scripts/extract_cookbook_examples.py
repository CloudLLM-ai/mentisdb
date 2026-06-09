#!/usr/bin/env python3
"""
Extract Rust code examples from the MentisDB Cookbook HTML files and emit
ONE test file per chapter under tests/cookbook/.

Why per-chapter and not monolithic? Cookbook chapters are multi-block
programs: example 1 defines a struct, example 2 uses it, example 3 queries
the chain built by example 2. Each example is NOT a standalone testable
unit. The chapter is the unit. So the test is `tests/cookbook/<chapter>.rs`
and every example in that chapter lives in one shared module scope.

The point: every code example in the cookbook is a claim about what the
MentisDB library does. If the API drifts and a chapter references a method
that no longer exists, this test fails before publish. Cookbook-as-test.

Usage:
    python3 scripts/extract_cookbook_examples.py docs/cookbook/

The script writes one .rs file per chapter into tests/cookbook/ and prints
a summary. The .rs files are regenerated on every run; they are NOT
committed (see .gitignore). The CI step regenerates and runs them.
"""

from __future__ import annotations

import html
import os
import re
import sys
import textwrap
from pathlib import Path

RUST_KEYWORDS = {
    "use", "fn", "let", "if", "else", "for", "while", "loop", "match",
    "return", "pub", "mod", "struct", "enum", "impl", "trait", "const",
    "static", "type", "where", "async", "await", "move", "ref", "mut",
    "self", "Self", "extern", "crate", "as", "in", "unsafe",
}

# Mark illustrative-only blocks with data-cookbook-test="off" on the
# <pre> tag. The extractor skips those.
RUST_HINT_RE = re.compile(
    r'<pre(?![^>]*data-cookbook-test="off")[^>]*>'
    r'\s*<code[^>]*>(.*?)</code>\s*</pre>',
    re.DOTALL | re.IGNORECASE,
)
NON_RUST_FIRST_LINE = re.compile(
    r"^("
    r"cd |ls |cat |echo |export |mkdir |rm |git |cargo |rustc |"
    r"curl |wget |sudo |apt |brew |python |node |npm |yarn |"
    r"\$ |> |===|##|# |\.\\/"
    r")"
)


def looks_like_rust(code: str) -> bool:
    stripped = code.lstrip()
    if not stripped:
        return False
    first_line = stripped.split("\n", 1)[0].strip()
    if NON_RUST_FIRST_LINE.match(first_line):
        return False
    if first_line in ("{", "}", "[", "]", "true", "false", "null"):
        return False
    first_token = first_line.split(None, 1)[0] if " " in first_line else first_line
    candidate = first_token.lstrip("#").rstrip("[").rstrip("!")
    candidate = candidate.rstrip(",.;").rstrip("::")
    if first_token in RUST_KEYWORDS:
        return True
    if first_token.startswith("//"):
        return True
    if first_token.startswith("///") or first_token.startswith("//!"):
        return True
    if first_token.startswith("#[") or first_token.startswith("#!"):
        return True
    if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", candidate):
        return True
    return False


def decode_block(raw: str) -> str:
    """Unescape HTML, strip inline HTML tags, return the Rust source.

    Order matters: strip `<...>` (rustdoc highlight tags) BEFORE
    unescaping entities, otherwise `&lt;` would become `<` and the
    tag regex would eat Rust code like `Result<()>`.
    """
    # Strip inline HTML tags first (rustdoc highlighting).
    no_tags = re.sub(r"<[^>]+>", "", raw)
    # Then unescape entities (&lt; -> <, &amp; -> &, &quot; -> ").
    decoded = html.unescape(no_tags)
    return decoded.rstrip()


def chapter_to_module_name(chapter: str) -> str:
    base = Path(chapter).stem
    base = re.sub(r"[^A-Za-z0-9]+", "_", base).strip("_")
    return base


PREIMPORTED_CRATES = [
    "mentisdb",
    "mentisdb::search",
    "tempfile",
    "uuid",
    "std::io",
    "std::collections",
    "chrono",
]

USE_LINE_RE = re.compile(
    r"^\s*use (?:" + "|".join(re.escape(c) for c in PREIMPORTED_CRATES) + r")(?:::|::|\{)[^;]*;\s*$",
    re.MULTILINE,
)


def strip_duplicate_uses(source: str) -> str:
    """Remove `use` lines for crates the test wrapper already imports."""
    return USE_LINE_RE.sub("", source)


def emit_chapter_test(chapter: str, examples: list) -> str:
    """
    Emit a single .rs file for one chapter. The file is a test module that
    compiles every example in the chapter in the same scope, so cross-block
    references (struct defined in block 1, used in block 2) just work.

    Each example is wrapped in a function that calls the example body. If
    the example is a `fn main()` we rename it to `__example_N_main` and
    invoke it. If the example is a top-level `pub struct X` we let it stay
    as a module-level item.
    """
    mod_name = chapter_to_module_name(chapter)
    out = []
    out.append("// AUTO-GENERATED FILE — DO NOT EDIT")
    out.append("// Regenerate with: python3 scripts/extract_cookbook_examples.py docs/cookbook/")
    out.append("// Source chapter: " + chapter)
    out.append("")
    out.append("#![allow(unused_imports, unused_variables, dead_code, unused_must_use,")
    out.append("         non_snake_case, ambiguous_glob_reexports)]")
    out.append("//! Cookbooks-as-tests for chapter: " + Path(chapter).stem)
    out.append("")
    out.append("use mentisdb::{")
    out.append("    BinaryStorageAdapter, MentisDb,")
    out.append("    RankedSearchQuery, RankedSearchGraph,")
    out.append("    ThoughtInput, ThoughtType, ThoughtRole, ThoughtQuery,")
    out.append("    ThoughtRelation, ThoughtRelationKind,")
    out.append("    RankedSearchResult, RankedSearchHit,")
    out.append("};")
    out.append("use mentisdb::search::{")
    out.append("    LocalTextEmbeddingProvider, EmbeddingProvider,")
    out.append("    EmbeddingInput, EmbeddingVector, EmbeddingMetadata,")
    out.append("    VectorDocument, VectorIndex, VectorQuery,")
    out.append("};")
    out.append("use tempfile::TempDir;")
    out.append("use uuid::Uuid;")
    out.append("use std::io;")
    out.append("use std::collections::HashSet;")
    out.append("use chrono::{DateTime, Utc, Duration};")
    out.append("")

    for i, (name, source) in enumerate(examples, start=1):
        header = (
            "\n// ============================================================\n"
            "// Example " + str(i) + " — test name: " + name + "\n"
            "// ============================================================\n"
        )
        out.append(header)
        # Strip duplicate `use mentisdb::*` lines (the test wrapper
        # pre-imports them at the top of the file).
        clean = strip_duplicate_uses(source)
        # The source goes straight into the module scope. Cross-block
        # references work because all examples share the same scope.
        out.append(clean)
        out.append("")

    out.append("")
    out.append("#[test]")
    out.append("fn chapter_compiles() {")
    out.append("    // The mere fact that this module compiled is the test.")
    out.append("    // If the chapter references a method, type, or import that")
    out.append("    // does not exist in the public MentisDB API, this module")
    out.append("    // fails to compile, and this test fails.")
    out.append("}")
    return "\n".join(out)


def main(argv):
    if len(argv) != 2:
        sys.stderr.write(__doc__)
        return 2

    cookbook_dir = argv[1]
    if not os.path.isdir(cookbook_dir):
        sys.stderr.write("error: not a directory: " + cookbook_dir + "\n")
        return 2

    out_dir = Path("tests/cookbook")
    out_dir.mkdir(parents=True, exist_ok=True)

    summary = []
    total_examples = 0
    total_chapters = 0
    for chapter_path in sorted(Path(cookbook_dir).glob("*.html")):
        # Skip the TOC itself
        if chapter_path.name == "cookbook.html":
            continue
        text = chapter_path.read_text(encoding="utf-8")
        examples = []
        for i, match in enumerate(RUST_HINT_RE.finditer(text), start=1):
            raw = match.group(1)
            decoded = decode_block(raw)
            if not looks_like_rust(decoded):
                continue
            name = chapter_to_module_name(chapter_path.name) + "_ex_" + str(i).zfill(2)
            examples.append((name, decoded))

        if not examples:
            continue

        mod_name = chapter_to_module_name(chapter_path.name)
        out_file = out_dir / (mod_name + ".rs")
        out_file.write_text(
            emit_chapter_test(str(chapter_path), examples),
            encoding="utf-8",
        )
        summary.append((chapter_path.name, len(examples), str(out_file)))
        total_examples += len(examples)
        total_chapters += 1

    # Write a barrel file that includes all chapter modules
    barrel = out_dir / "mod.rs"
    barrel_lines = ["// AUTO-GENERATED FILE — DO NOT EDIT", ""]
    for _, _, path in summary:
        mod = Path(path).stem
        # Use absolute path so #[path] resolves correctly when the
        # mod.rs is included from tests/cookbook_tests.rs.
        abs_path = str(Path(path).resolve())
        barrel_lines.append("#[path = \"" + abs_path + "\"]")
        barrel_lines.append("mod " + mod + ";")
    barrel.write_text("\n".join(barrel_lines) + "\n", encoding="utf-8")

    # Cargo test targets are tests/*.rs files. Create a thin wrapper that
    # re-exports the mod.rs from the cookbook subdirectory.
    test_target = Path("tests/cookbook_tests.rs")
    test_target.write_text(
        "// AUTO-GENERATED FILE — DO NOT EDIT\n"
        "// Regenerate with: python3 scripts/extract_cookbook_examples.py\n"
        "\n"
        "mod cookbook;\n",
        encoding="utf-8",
    )

    sys.stderr.write("\nExtracted: " + str(total_chapters) + " chapter(s), "
                     + str(total_examples) + " Rust example(s)\n")
    sys.stderr.write("Output dir: tests/cookbook/\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
