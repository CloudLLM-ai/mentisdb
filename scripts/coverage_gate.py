#!/usr/bin/env python3
"""Coverage table and gate: read the JSON that `cargo llvm-cov report --json
--summary-only` writes, print a Markdown table of functions, branches, regions
and lines, and exit 1 when a dimension is under its floor.

Usage: coverage_gate.py SUMMARY.json [--goal PERCENT] [--floor METRIC=PERCENT ...]
                        [--toolchain TEXT] [--worst N] [--markdown PATH]

METRIC is one of functions, branches, regions, lines. Two numbers per metric:

* the goal (`--goal`, 90) is where the project wants to be; the table shows
  whether each metric meets it, and a miss never fails the run;
* the floor (`--floor`) is what the gate enforces: the measured value, rounded
  down, at the time it was set. It only moves up (docs/ENGINEERING.md,
  "Coverage"). A metric with no floor is shown but not gated; a floor on a
  metric that has no data in the report is an error, not a silent pass.

`--toolchain` is printed under the table so a reader knows which compiler
produced the numbers. The table goes to stdout and to `--markdown`, which CI
appends to $GITHUB_STEP_SUMMARY.
"""
import argparse
import json
import sys

METRICS = ("functions", "branches", "regions", "lines")


def percent(entry):
    return 100.0 * entry["covered"] / entry["count"] if entry["count"] else 100.0


def short_name(path):
    return "src/" + path.split("/src/", 1)[-1] if "/src/" in path else path


def metric_row(name, entry, goal, floor, failed):
    value = percent(entry)
    met = "✅" if value >= goal else "❌"
    if floor is None:
        gate = "not gated"
    elif value >= floor:
        gate = f"floor {floor:.0f}% ok"
    else:
        gate = f"**FAIL** below floor {floor:.0f}%"
        failed.append(f"{name} {value:.2f}% is below its floor of {floor:.0f}%")
    covered, count = entry["covered"], entry["count"]
    return f"| {name} | {value:.2f}% | {covered:,} / {count:,} | {goal:.0f}% {met} | {gate} |"


def uncovered_table(files, worst):
    ranked = sorted(
        files,
        key=lambda f: f["summary"]["regions"]["count"] - f["summary"]["regions"]["covered"],
        reverse=True,
    )[:worst]
    rows = [
        "\nMost uncovered files (by uncovered regions):\n",
        "| file | uncovered regions | regions | functions | lines |",
        "|---|---:|---:|---:|---:|",
    ]
    for f in ranked:
        s = f["summary"]
        missed = s["regions"]["count"] - s["regions"]["covered"]
        rows.append(
            f"| {short_name(f['filename'])} | {missed:,} | {percent(s['regions']):.1f}% | "
            f"{percent(s['functions']):.1f}% | {percent(s['lines']):.1f}% |"
        )
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("summary")
    parser.add_argument("--goal", type=float, default=90.0, help="target percent for every metric")
    parser.add_argument("--floor", action="append", default=[], metavar="METRIC=PERCENT")
    parser.add_argument("--toolchain", default="", help="text naming the toolchain that measured")
    parser.add_argument("--worst", type=int, default=10, help="files to list, most uncovered first")
    parser.add_argument("--markdown", metavar="PATH", help="also write the table to PATH")
    args = parser.parse_args()

    floors = {}
    for item in args.floor:
        name, _, value = item.partition("=")
        if name not in METRICS or not value:
            parser.error(f"--floor wants METRIC=PERCENT with METRIC in {METRICS}, got {item!r}")
        floors[name] = float(value)

    with open(args.summary) as handle:
        data = json.load(handle)["data"][0]
    totals = data["totals"]
    failed = []
    out = ["## Test coverage\n", "| dimension | percent | covered / total | goal | gate |", "|---|---:|---:|---:|---|"]
    for name in METRICS:
        entry = totals.get(name)
        if entry is None or entry["count"] == 0:
            if name in floors:
                failed.append(f"{name}: no data in the report but a floor was set")
            continue
        out.append(metric_row(name, entry, args.goal, floors.get(name), failed))
    if args.toolchain:
        out.append(f"\nAll four dimensions come from one instrumented run on {args.toolchain}.")
    out.extend(uncovered_table(data["files"], args.worst))

    text = "\n".join(out) + "\n"
    sys.stdout.write(text)
    if args.markdown:
        with open(args.markdown, "w") as handle:
            handle.write(text)

    if failed:
        print("\nCoverage gate failed:\n", file=sys.stderr)
        for line in failed:
            print(f"  - {line}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
