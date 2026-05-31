#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

make -C tests/macho
mkdir -p testout
cargo run macho -i tests/macho/helloworld -o testout/
wc -c testout/decompressor
wc -c testout/decompressor.sh
