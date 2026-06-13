#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

make -C tests/macho
mkdir -p testout
rm -rf testout/macho
cargo run macho -i tests/macho/helloworld -o testout/macho
wc -c testout/macho/decompressor
wc -c testout/macho/decompressor.sh
