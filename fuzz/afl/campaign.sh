#!/bin/sh
# Runs every target for a fixed time, one AFL instance per target, in
# parallel, and reports crashes and hangs.
#
# Usage: fuzz/afl/campaign.sh [seconds] [target...]
#
# Targets read the prepared seeds (run prepare-seeds.sh first) when they exist
# and their committed regressions otherwise. Findings land in
# findings/<target>.

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"
seconds=${1:-300}
[ "$#" -gt 0 ] && shift
targets=${*:-"decompress roundtrip stream_compress"}

cargo afl build --release
mkdir -p findings
for target in $targets; do
    # roundtrip reads a configuration header and a payload; stream_compress
    # adds four control bytes; decompress reads a frame.
    seeds="regressions/$target"
    case "$target" in
        decompress) [ -d seeds/frames ] && seeds=seeds/frames ;;
        roundtrip) [ -d seeds/params ] && seeds=seeds/params ;;
        stream_compress) [ -d seeds/stream ] && seeds=seeds/stream ;;
    esac
    rm -rf "findings/$target"
    # A seed that crashes (a mutated frame among the decoder seeds can) is
    # skipped rather than aborting the instance; fuzzing finds it again.
    AFL_SKIP_CPUFREQ=1 AFL_NO_UI=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 AFL_IGNORE_SEED_PROBLEMS=1 \
        cargo afl fuzz -V "$seconds" -t 20000 -i "$seeds" -o "findings/$target" \
        "target/release/$target" > "findings/$target.log" 2>&1 &
done
wait

status=0
for target in $targets; do
    # AFL writes fuzzer_stats once the target runs; without it the instance
    # failed to start (on many hosts: run `cargo afl system-config` first).
    if [ ! -f "findings/$target/default/fuzzer_stats" ]; then
        echo "$target: AFL did not start; see findings/$target.log"
        status=1
        continue
    fi
    crashes=$(find "findings/$target/default/crashes" -type f -name 'id:*' 2>/dev/null | wc -l | tr -d ' ')
    hangs=$(find "findings/$target/default/hangs" -type f -name 'id:*' 2>/dev/null | wc -l | tr -d ' ')
    summary=$(grep -a 'Statistics' "findings/$target.log" | tail -1 | sed 's/.*Statistics: //; s/\x1b\[[0-9;]*m//g')
    echo "$target: $crashes crashes, $hangs hangs; $summary"
    [ "$crashes" -eq 0 ] || status=1
done
exit "$status"
