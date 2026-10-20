#!/bin/sh
# Builds the wasm fixtures of the report tests from fixture.c (needs zig and wabt).
set -e
cd "$(dirname "$0")"
zig cc --target=wasm32-freestanding -O2 -g -nostdlib -Wl,--no-entry -o fixture.debug.wasm fixture.c
cp fixture.debug.wasm fixture.wasm
wasm-strip fixture.wasm
