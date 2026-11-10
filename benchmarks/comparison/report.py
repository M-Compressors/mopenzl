#!/usr/bin/env python3
"""Render the Silesia comparison: SVG charts and a Markdown report.

    report.py --csv results.csv --output docs/benchmarks \
              --subtitle "AMD Ryzen 9 7950X · rustc 1.99.1 · 2026-11-09"

`results.csv` is written by `mopenzl-comparison run --out`. Every pair in it
was verified byte-identical with C before it was timed, so compressed sizes
are equal on both sides by construction.

Writes `charts/overview.svg` (OpenZL C and mopenzl throughput per graph, in
MB/s, compress and decompress), `charts/<case>.svg` (the same per file) and
`README.md` (tables). Throughput is of uncompressed bytes; a graph's figure is
its total bytes over its total median time. Requires matplotlib==3.10.8.
"""

import argparse
import csv
import statistics
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

MB = 1e6
RUST, C = "#D55E00", "#0072B2"
SIDES = (("c", C, "OpenZL C"), ("rust", RUST, "mopenzl"))
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


def mbs(cell, side, op):
    """MB/s of uncompressed bytes for one file."""
    return cell["bytes"] / cell[(side, op)] * 1e9 / MB


def ratio(cell, op):
    """C time / Rust time: above 1 means Rust is faster."""
    return cell[("c", op)] / cell[("rust", op)]


def rows_of(cells, case):
    return [(f, cell) for (c, f), cell in cells.items() if c == case]


def total_mbs(cells, case, side, op):
    """A graph's throughput: its total bytes over its total median time."""
    rows = rows_of(cells, case)
    return sum(cell["bytes"] for _, cell in rows) / sum(cell[(side, op)] for _, cell in rows) * 1e9 / MB


def mb_label(value):
    return f"{value:,.0f}" if value >= 100 else f"{value:.1f}"


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


def overview(cells, path, subtitle):
    style()
    names = cases(cells)
    counts = {name: len(rows_of(cells, name)) for name in names}
    fig, axes = plt.subplots(1, 2, figsize=(16, 10.5), sharey=True)
    height = .38
    for ax, op in zip(axes, OPS):
        values = {side: [total_mbs(cells, name, side, op) for name in names] for side, _, _ in SIDES}
        for index, (side, color, label) in enumerate(SIDES):
            positions = [i + (index - .5) * height for i in range(len(names))]
            bars = ax.barh(positions, values[side], height=height * .92, color=color, label=label)
            ax.bar_label(bars, labels=[mb_label(v) for v in values[side]], padding=3, fontsize=8.5, color="#334155")
        ax.set_xscale("log")
        low = min(min(v) for v in values.values())
        high = max(max(v) for v in values.values())
        ax.set_xlim(low / 2, high * 3.2)
        ax.set_xlabel(f"{op.capitalize()} throughput, MB/s (log scale) · higher is better")
        ax.set_title(op.capitalize(), fontsize=14, loc="left")
        ax.grid(axis="x", which="major", alpha=.18)
        ax.set_axisbelow(True)
        ax.tick_params(axis="y", length=0)
    axes[0].set_yticks(range(len(names)), [f"{name} ({counts[name]})" for name in names], fontsize=10.5)
    axes[0].invert_yaxis()
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="upper center", bbox_to_anchor=(.5, .945), ncol=2, frameon=False, fontsize=11)
    fig.suptitle("OpenZL C vs mopenzl on Silesia · throughput per graph", fontsize=17)
    fig.text(.5, .012, subtitle + "\n(n) = Silesia files in the graph; MB/s = 10⁶ uncompressed bytes per second, "
             "total bytes / total median time · frames byte-identical, so compressed sizes are equal",
             ha="center", fontsize=9.5)
    fig.tight_layout(rect=(0, .05, 1, .93))
    save_svg(fig, path)
    plt.close(fig)


def per_case(cells, case, path, subtitle):
    style()
    rows = rows_of(cells, case)
    files = [f for f, _ in rows]
    fig, axes = plt.subplots(1, 2, figsize=(15, 5.4))
    width = .38
    for ax, op in zip(axes, OPS):
        for index, (side, color, label) in enumerate(SIDES):
            values = [mbs(cell, side, op) for _, cell in rows]
            positions = [i + (index - .5) * width for i in range(len(files))]
            bars = ax.bar(positions, values, width=width * .92, color=color, label=label)
            if len(files) <= 3:
                ax.bar_label(bars, labels=[mb_label(v) for v in values], padding=2, fontsize=8.5, color="#334155")
        ax.set_xticks(range(len(files)), files, rotation=30, ha="right")
        ax.set_ylabel(f"{op.capitalize()} MB/s · higher is better")
        ax.grid(axis="y", alpha=.18)
        ax.set_axisbelow(True)
        c_total, rust_total = (total_mbs(cells, case, side, op) for side in ("c", "rust"))
        ax.set_title(f"{op} · C {mb_label(c_total)} MB/s, mopenzl {mb_label(rust_total)} MB/s", fontsize=12)
    axes[0].legend(frameon=False)
    fig.suptitle(f"{case} graph · OpenZL C vs mopenzl per Silesia file", fontsize=15)
    fig.text(.5, .01, subtitle, ha="center", fontsize=9)
    fig.tight_layout(rect=(0, .04, 1, .95))
    save_svg(fig, path)
    plt.close(fig)


def markdown(cells, subtitle, raw=None):
    lines = [
        "# Silesia comparison",
        "",
        f"**{subtitle}.** Each standard graph compresses every listed Silesia file as one",
        "input with the library defaults (level 6, format version 27, both checksums).",
        "Every frame was verified byte-identical to OpenZL v0.3.0 C, and both decoders",
        "restored the input, before timing; compressed sizes are therefore equal.",
        "Throughput is MB/s (10⁶ bytes per second) of uncompressed bytes, from the",
        "median of alternating rounds; a graph's figure is its total bytes over its",
        "total median time. \"/ C\" is mopenzl's speed relative to C (above 1× is faster).",
        "",
        "![Throughput per graph, OpenZL C and mopenzl](charts/overview.svg)",
        "",
        "Raw measurements: " + (f"[{raw}]({raw}) · " if raw else "")
        + "[how to reproduce](../benchmarking.md#silesia-comparison).",
        "",
        "## Per graph",
        "",
        "| Graph | Files | C compress | mopenzl compress | / C | C decompress | mopenzl decompress | / C |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for case in cases(cells):
        row = f"| {case} | {len(rows_of(cells, case))} |"
        for op in OPS:
            c_total, rust_total = (total_mbs(cells, case, side, op) for side in ("c", "rust"))
            row += f" {mb_label(c_total)} MB/s | {mb_label(rust_total)} MB/s | {rust_total / c_total:.2f}× |"
        lines.append(row)
    for case in cases(cells):
        lines += ["", f"## {case}", "", f"![{case}](charts/{case}.svg)", "",
                  "| File | Bytes | Ratio | C compress | mopenzl compress | / C | C decompress | mopenzl decompress | / C |",
                  "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        for f, cell in rows_of(cells, case):
            lines.append(
                f"| {f} | {cell['bytes']} | {cell['bytes'] / cell['compressed']:.3f} | "
                f"{mb_label(mbs(cell, 'c', 'compress'))} MB/s | {mb_label(mbs(cell, 'rust', 'compress'))} MB/s | "
                f"{ratio(cell, 'compress'):.2f}× | {mb_label(mbs(cell, 'c', 'decompress'))} MB/s | "
                f"{mb_label(mbs(cell, 'rust', 'decompress'))} MB/s | {ratio(cell, 'decompress'):.2f}× |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--subtitle", default="Implementation comparison")
    parser.add_argument("--raw", help="link to the raw CSVs, relative to the output directory")
    args = parser.parse_args()
    cells = load(args.csv)
    charts = args.output / "charts"
    charts.mkdir(parents=True, exist_ok=True)
    overview(cells, charts / "overview.svg", args.subtitle)
    for case in cases(cells):
        per_case(cells, case, charts / f"{case}.svg", args.subtitle)
    (args.output / "README.md").write_text(markdown(cells, args.subtitle, args.raw))


if __name__ == "__main__":
    main()
