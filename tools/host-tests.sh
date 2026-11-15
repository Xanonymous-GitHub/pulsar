#!/bin/sh
# Runs the unit tests that work on the host: the modules that import nothing from the crate
# (`cargo test` cannot run, the crate only builds for the board). Uses the pinned toolchain.
set -eu

cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

# One program per file. tools/linux-host.rs holds the three files of the Linux machine
# (cpu.rs, fdt.rs, machine.rs), which need each other.
for file in \
    src/mqtt/topic.rs \
    src/mqtt/reassemble.rs \
    src/mqtt/budget.rs \
    src/telemetry/confirmation.rs \
    src/handshake.rs \
    tools/linux-host.rs
do
    echo "== $file"
    rustc --edition 2024 --test "$file" -o "$out/tests"
    "$out/tests" --quiet
done
