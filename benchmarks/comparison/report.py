#!/usr/bin/env python3
"""Render the Silesia comparison: SVG charts and a Markdown report.

    report.py --csv after.csv [--before before.csv] --output docs/benchmarks \
              --subtitle "AMD Ryzen 9 7950X · rustc 1.99.1 · 2026-11-09"

`after.csv` (and the optional `before.csv`, the same matrix measured on the
revision before the change) are written by `mopenzl-comparison run --out`.
Every pair in them was verified byte-identical with C before it was timed,
so compressed sizes are equal on both sides by construction.

Writes `charts/overview.svg` (median Rust/C speed ratio per graph, compress
and decompress, before and after), `charts/<case>.svg` (per-file throughput
of both sides) and `README.md` (tables). Requires matplotlib==3.10.8.
"""

import argparse
import csv
import statistics
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

MIB = 1 << 20
RUST, C, BEFORE = "#D55E00", "#0072B2", "#E69F00"
OPS = ("compress", "decompress")


def load(path):
    """{(case, file): {"bytes", "compressed", (side, op): median_ns}} in file order."""
    cells = {}
    with path.open() as source:
        for row in csv.DictReader(source):
            if row["op"] == "error":
                raise SystemExit(f"{path}: {row['case']}/{row['file']} failed verification")
            cell = cells.setdefault((row["case"], row["file"]), {
                "bytes": int(row["bytes"]), "compressed": int(row["compressed"])})
            cell[(row["side"], row["op"])] = float(row["median_ns"])
    return cells


def cases(cells):
    return list(dict.fromkeys(case for case, _ in cells))


def mibs(cell, side, op):
    return cell["bytes"] / cell[(side, op)] * 1e9 / MIB


def ratio(cell, op):
    """C time / Rust time: above 1 means Rust is faster."""
    return cell[("c", op)] / cell[("rust", op)]


def medians(cells, case, op):
    return statistics.median(ratio(cell, op) for (c, _), cell in cells.items() if c == case)


def style():
    plt.rcParams.update({
        "font.size": 11, "axes.spines.top": False, "axes.spines.right": False,
        "svg.fonttype": "none", "svg.hashsalt": "mopenzl-comparison",
        "figure.facecolor": "white",
    })


def save_svg(fig, path):
    """Deterministic SVG without trailing whitespace."""
    fig.savefig(path, metadata={"Date": None})
    path.write_text("\n".join(line.rstrip() for line in path.read_text().splitlines()) + "\n")


def overview(after, before, path, subtitle):
    style()
    names = cases(after)
    fig, axes = plt.subplots(2, 1, figsize=(16, 9.5))
    series = ([("before", before, BEFORE)] if before else []) + [("after", after, RUST)]
    width = .76 / len(series)
    for ax, op in zip(axes, OPS):
        top = 1.0
        for index, (label, cells, color) in enumerate(series):
            values = [medians(cells, name, op) if any(c == name for c, _ in cells) else 0 for name in names]
            top = max(top, *values)
            positions = [i + (index - (len(series) - 1) / 2) * width for i in range(len(names))]
            bars = ax.bar(positions, values, width=width * .95, color=color,
                          label="mopenzl " + ("before" if label == "before" else "now"))
            if label == "after":
                ax.bar_label(bars, labels=[f"{v:.2f}×" for v in values], padding=3, fontsize=8.5)
        ax.set_xticks(range(len(names)), names, rotation=25, ha="right", fontsize=10)
        ax.set_ylim(0, top * 1.18)
        ax.set_ylabel(f"{op.capitalize()} speed / C\nhigher is better")
        ax.axhline(1, color="#64748b", linestyle="--", linewidth=1)
        ax.grid(axis="y", alpha=.18)
        ax.set_axisbelow(True)
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="upper center", bbox_to_anchor=(.5, .945), ncol=len(labels), frameon=False)
    fig.suptitle("mopenzl vs OpenZL C on Silesia · median per-file speed ratio per graph", fontsize=17)
    fig.text(.5, .015, subtitle + "\nDashed line: C = 1× · frames byte-identical, so compressed sizes are equal",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .05, 1, .92))
    save_svg(fig, path)
    plt.close(fig)


def per_case(after, case, path, subtitle):
    style()
    rows = [(f, cell) for (c, f), cell in after.items() if c == case]
    files = [f for f, _ in rows]
    fig, axes = plt.subplots(1, 2, figsize=(15, 5))
    for ax, op in zip(axes, OPS):
        width = .38
        for index, (side, color, label) in enumerate([("rust", RUST, "mopenzl"), ("c", C, "OpenZL C")]):
            values = [mibs(cell, side, op) for _, cell in rows]
            positions = [i + (index - .5) * width for i in range(len(files))]
            ax.bar(positions, values, width=width * .95, color=color, label=label)
        ax.set_xticks(range(len(files)), files, rotation=30, ha="right")
        ax.set_ylabel(f"{op.capitalize()} MiB/s · higher is better")
        ax.grid(axis="y", alpha=.18)
        ax.set_axisbelow(True)
        ax.set_title(f"{op} · median Rust/C {medians(after, case, op):.2f}×")
    axes[0].legend(frameon=False)
    fig.suptitle(f"{case} graph · Silesia files", fontsize=15)
    fig.text(.5, .01, subtitle, ha="center", fontsize=9)
    fig.tight_layout(rect=(0, .04, 1, .95))
    save_svg(fig, path)
    plt.close(fig)


def markdown(after, before, subtitle, raw=None):
    lines = [
        "# Silesia comparison",
        "",
        f"**{subtitle}.** Each standard graph compresses every listed Silesia file as one",
        "input with the library defaults (level 6, format version 27, both checksums).",
        "Every frame was verified byte-identical to OpenZL v0.3.0 C, and both decoders",
        "restored the input, before timing; compressed sizes are therefore equal.",
        "Speed ratio is C time / Rust time (above 1× means Rust is faster), the median",
        "of alternating rounds. Throughput is of uncompressed bytes.",
        "",
        "![Median speed ratio per graph](charts/overview.svg)",
        "",
        "Raw measurements: " + (f"[{raw}]({raw}) · " if raw else "")
        + "[how to reproduce](../benchmarking.md#silesia-comparison).",
        "",
        "## Medians per graph",
        "",
        "| Graph | Files | Compress / C | Decompress / C |" + (" Before: compress / C | Before: decompress / C |" if before else ""),
        "| --- | --- | --- | --- |" + (" --- | --- |" if before else ""),
    ]
    for case in cases(after):
        count = sum(1 for c, _ in after if c == case)
        row = f"| {case} | {count} | {medians(after, case, 'compress'):.3f}× | {medians(after, case, 'decompress'):.3f}× |"
        if before:
            row += (f" {medians(before, case, 'compress'):.3f}× | {medians(before, case, 'decompress'):.3f}× |"
                    if any(c == case for c, _ in before) else " — | — |")
        lines.append(row)
    for case in cases(after):
        lines += ["", f"## {case}", "", f"![{case}](charts/{case}.svg)", "",
                  "| File | Bytes | Ratio | Rust compress | C compress | / C | Rust decompress | C decompress | / C |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        for (c, f), cell in after.items():
            if c != case:
                continue
            lines.append(
                f"| {f} | {cell['bytes']} | {cell['bytes'] / cell['compressed']:.3f} | "
                f"{mibs(cell, 'rust', 'compress'):.1f} MiB/s | {mibs(cell, 'c', 'compress'):.1f} MiB/s | "
                f"{ratio(cell, 'compress'):.3f}× | {mibs(cell, 'rust', 'decompress'):.1f} MiB/s | "
                f"{mibs(cell, 'c', 'decompress'):.1f} MiB/s | {ratio(cell, 'decompress'):.3f}× |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--before", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--subtitle", default="Implementation comparison")
    parser.add_argument("--raw", help="link to the raw CSVs, relative to the output directory")
    args = parser.parse_args()
    after = load(args.csv)
    before = load(args.before) if args.before else None
    charts = args.output / "charts"
    charts.mkdir(parents=True, exist_ok=True)
    overview(after, before, charts / "overview.svg", args.subtitle)
    for case in cases(after):
        per_case(after, case, charts / f"{case}.svg", args.subtitle)
    (args.output / "README.md").write_text(markdown(after, before, args.subtitle, args.raw))


if __name__ == "__main__":
    main()
