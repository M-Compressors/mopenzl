#!/usr/bin/env python3
"""Run the SIMD and uninitialized-memory tests under Miri or AddressSanitizer.

Tests are chosen by name prefix from the binary's own `--list`, then run with
`--exact`, so a prefix cannot also match an unrelated module by substring, and
a prefix that selects no test, or whose every test is ignored, fails the run
instead of passing silently.

    scripts/check_simd_uninit.py miri [--target TRIPLE] [--jobs N] [--batch N] [--shard K/N]
    scripts/check_simd_uninit.py asan [--target TRIPLE] [--jobs N] [--batch N] [--shard K/N]

`--miri` and `--asan` are accepted in place of the positional tool.

mopenzl's only `unsafe` is behind its dependencies: `fearless_simd` for the
XXH3 accumulator in `src/shared/xxh3.rs`, the one vector kernel, dispatched
over every backend the host offers, and the bookkeeping of the destinations
that write into `MaybeUninit` output (`compress_to_uninit`,
`decompress_to_uninit`, `process_uninit`). Their unit-test modules, the
`simd_backends` integration test, which runs both codecs on every backend,
and the `uninit` integration test (including custom-codec destinations) are
what this script checks.

Miri cannot call into C, so tests that compare against `openzl-ffi` do not
run under Miri: lib tests are skipped by exact name (`MIRI_SKIPS`), and in
`uninit` only the tests named `rust_only_*` run. Everything runs under ASan.
A skipped name that no longer exists fails the run, so a rename cannot turn a
skip into a silent hole.

Each process runs a batch of up to `--batch` tests: starting `cargo miri` costs
seconds. `--shard K/N` runs every N-th test, starting from the K-th, so CI can
split the run across runners.

Miri reaches only the SIMD levels the build enables at compile time; set
`RUSTFLAGS=-Ctarget-cpu=...` (and a separate `CARGO_TARGET_DIR`) to select
them. AddressSanitizer runs natively and reaches every level the host
detects.
"""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import argparse
import math
import os
import re
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[1]

# Unit-test modules that hold a SIMD kernel, dispatch one across backends, or
# write to or read from `MaybeUninit` memory.
LIB = [
    "compressor::core::codecs::field_lz::",
    "compressor::core::codecs::lz_common::count::",
    "compressor::core::codecs::lz_common::row_table::",
    "compressor::core::codecs::pivco_huffman::tests::",
    "compressor::core::sink::",
    "decompressor::core::codecs::entropy::",
    "decompressor::core::codecs::field_lz::",
    "decompressor::core::codecs::lz_copy::",
    "decompressor::core::codecs::pivco_huffman::kernels::tests::",
    "decompressor::core::codecs::pivco_huffman::tests::",
    "decompressor::core::out::",
    "shared::bitpack_kernels::tests::bit_count",
    "shared::bitpack_kernels::tests::grouped_encode",
    "shared::byte_transpose::tests::",
    "shared::destination::",
    "shared::entropy::",
    "shared::xxh3::tests::",
]

# Integration tests: the binary and the name prefix of the tests to run.
# Some `uninit` tests compare against the C oracle, which Miri cannot call;
# the ones that do not are named `rust_only_`.
TESTS = {
    "miri": [
        ("simd_backends", ""),
        ("uninit", "rust_only_"),
    ],
    "asan": [
        ("simd_backends", ""),
        ("uninit", ""),
    ],
}

# Tests that call `openzl_ffi`, which Miri cannot interpret: it has no shim
# for foreign C functions. They still run under AddressSanitizer.
MIRI_SKIPS = [
    "shared::xxh3::tests::every_length_class_matches_the_reference_library",
]

# One line per test of libtest's pretty output, with `--report-time`.
OUTCOME = re.compile(r"^test (.+) \.\.\. (ok|FAILED|ignored)(?: <([\d.]+)s>)?", re.MULTILINE)
# Arguments every test process passes to libtest. `--report-time` is
# unstable; under Miri it reports Miri's virtual clock, which counts executed
# work rather than wall time but ranks the tests all the same.
LIBTEST = ["--color", "never", "-Zunstable-options", "--report-time"]


def host() -> str:
    output = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True)
    return next(line.split()[1] for line in output.stdout.splitlines() if line.startswith("host:"))


def cargo(tool: str, target: str, selection: list[str]) -> list[str]:
    if tool == "miri":
        return ["cargo", "+nightly", "miri", "test", "--locked", "--target", target, *selection]
    return ["cargo", "+nightly", "test", "--locked", "--release", "--target", target, *selection]


def environment(tool: str) -> dict[str, str]:
    env = dict(os.environ)
    if tool == "miri":
        # A doc test built without the library's target features calls its
        # SIMD kernels without them, which Miri reports as undefined behavior.
        if "RUSTFLAGS" in env and "RUSTDOCFLAGS" not in env:
            env["RUSTDOCFLAGS"] = env["RUSTFLAGS"]
    else:
        for name in ["RUSTFLAGS", "RUSTDOCFLAGS"]:
            flags = env.get(name, "")
            if "-Zsanitizer=address" not in flags:
                env[name] = f"{flags} -Zsanitizer=address".strip()
        # The sanitized build gets a directory of its own, so it neither
        # invalidates nor reuses the ordinary release artifacts.
        env.setdefault("CARGO_TARGET_DIR", str(root / "target" / "asan"))
    return env


def listed(tool: str, target: str, env: dict[str, str], selection: list[str],
           ignored: bool = False) -> list[str]:
    command = cargo(tool, target, selection) + ["--", "--list", "--format", "terse"]
    if ignored:
        command.append("--ignored")
    output = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
    if output.returncode != 0:
        sys.exit(f"listing {' '.join(selection)} failed:\n{output.stderr}")
    return [line.removesuffix(": test") for line in output.stdout.splitlines()
            if line.endswith(": test")]


def runnable(tool: str, target: str, env: dict[str, str], selection: list[str]) -> list[str]:
    """Lists the tests of one binary that this configuration does not ignore."""
    ignored = set(listed(tool, target, env, selection, ignored=True))
    return [name for name in listed(tool, target, env, selection) if name not in ignored]


def run(command: list[str], env: dict[str, str]) -> tuple[int, str, str, float]:
    """Runs one test command; returns its exit code, its standard output, its
    whole log and its wall time in seconds."""
    start = time.monotonic()
    output = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
    return (output.returncode, output.stdout, output.stdout + output.stderr,
            time.monotonic() - start)


def shard(value: str) -> tuple[int, int]:
    index, _, count = value.partition("/")
    try:
        index, count = int(index), int(count)
    except ValueError:
        raise argparse.ArgumentTypeError(f"expected K/N, got {value!r}")
    if not 1 <= index <= count:
        raise argparse.ArgumentTypeError(f"shard {index} is outside 1..{count}")
    return index, count


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("tool", nargs="?", choices=["miri", "asan"])
    parser.add_argument("--miri", dest="flag", action="store_const", const="miri",
                        help="same as the positional `miri`")
    parser.add_argument("--asan", dest="flag", action="store_const", const="asan",
                        help="same as the positional `asan`")
    parser.add_argument("--target", default=host())
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--batch", type=int, default=4, help="tests per process")
    parser.add_argument("--shard", type=shard, default=(1, 1), help="K/N: run the K-th of N shards")
    parser.add_argument("--slowest", type=int, default=10, help="slow tests to report")
    args = parser.parse_args()
    if args.tool and args.flag and args.tool != args.flag:
        parser.error(f"both {args.tool} and --{args.flag} given")
    tool = args.tool or args.flag
    if tool is None:
        parser.error("choose a tool: miri or asan (or --miri / --asan)")
    env = environment(tool)
    start = time.monotonic()

    tests = TESTS[tool]
    binaries = sorted({binary for binary, _ in tests})
    build = cargo(tool, args.target, ["--lib", "--no-run"])
    build += [arg for name in binaries for arg in ["--test", name]]
    subprocess.run(build, cwd=root, env=env, check=True)

    selections = [["--lib"]] + [["--test", name] for name in binaries]
    with ThreadPoolExecutor(args.jobs) as pool:
        names = dict(zip(map(tuple, selections), pool.map(
            lambda selection: runnable(tool, args.target, env, selection), selections)))

    every = {name for chosen in names.values() for name in chosen}
    stale = [name for name in MIRI_SKIPS if name not in every]
    if stale:
        sys.exit(f"MIRI_SKIPS names tests that no longer exist: {', '.join(stale)}")
    skips = set(MIRI_SKIPS) if tool == "miri" else set()

    # Every selected test, under the prefix that chose it.
    groups: dict[str, list[tuple[tuple[str, ...], str]]] = {}
    for prefix in LIB:
        groups[prefix] = [(("--lib",), name) for name in names[("--lib",)]
                          if name.startswith(prefix) and name not in skips]
    for binary, prefix in tests:
        selection = ("--test", binary)
        groups[f"{binary}::{prefix}"] = [(selection, name) for name in names[selection]
                                         if name.startswith(prefix) and name not in skips]
    empty = [label for label, chosen in groups.items() if not chosen]
    if empty:
        sys.exit(f"no test that runs here matches: {', '.join(empty)}")

    index, count = args.shard
    dealt = list(dict.fromkeys(test for chosen in groups.values() for test in chosen))
    mine = [test for position, test in enumerate(dealt) if position % count == index - 1]
    jobs: list[tuple[list[str], list[tuple[tuple[str, ...], str]]]] = []
    for selection in names:
        chosen = [test for test in mine if test[0] == selection]
        batches = math.ceil(len(chosen) / max(args.batch, 1))
        for first in range(batches):
            batch = chosen[first::batches]
            jobs.append((cargo(tool, args.target, [
                *selection, "--", *LIBTEST, "--exact", *[name for _, name in batch]
            ]), batch))

    # The longest batches first, so the pool does not end on one.
    jobs.sort(key=lambda job: -len(job[1]))
    with ThreadPoolExecutor(args.jobs) as pool:
        results = list(pool.map(lambda job: run(job[0], env), jobs))

    failed = False
    outcomes: dict[tuple[tuple[str, ...], str], str] = {}
    timings: list[tuple[float, str]] = []
    for (command, batch), (code, stdout, log, _) in zip(jobs, results):
        lines = OUTCOME.findall(stdout)
        reported = {name: outcome for name, outcome, _ in lines}
        timings += [(float(seconds), name) for name, _, seconds in lines if seconds]
        for test in batch:
            outcomes[test] = reported.get(test[1], "not run")
        if code != 0:
            print(log, file=sys.stderr)
    for label, chosen in groups.items():
        here = [test for test in chosen if test in outcomes]
        if not here:
            continue
        broken = [name for selection, name in here if outcomes[(selection, name)] != "ok"]
        passed = len(here) - len(broken)
        if broken:
            failed = True
            print(f"FAIL {label}: {passed} passed, {len(broken)} failed: {', '.join(broken)}")
        else:
            print(f"ok   {label}: {passed} passed")
    if skips:
        print(f"skipped under Miri (they call C): {', '.join(sorted(skips))}")
    print(f"{tool} shard {index}/{count}: {len(mine)} tests in {len(jobs)} processes, "
          f"{time.monotonic() - start:.0f}s")
    print("slowest tests, by libtest's clock:")
    for seconds, name in sorted(timings, reverse=True)[:args.slowest]:
        print(f"  {seconds:7.1f}s {name}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
