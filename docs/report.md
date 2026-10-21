# Compression report

The report shows where the bytes of your intro go, so you know what to change to make it smaller.

## Options
All of these turn on the report:

- `--report`, `-r`: Write the report to `report/` in the output directory.
- `--report-models`: Also record what every submodel predicted.
- `--report-detail <VIEW,...>`: Also write these views of the report as TSV files, e.g. `lines,tokens`.
- `--report-source-map <FILE>`: Source map of the main JavaScript, to map its costs back to the original source files and lines.
- `--report-label <FILE>=<LABEL>`: Name of a file in the report, e.g. the module it was made from. Repeat the option for each file.
- `--report-wasm-debug <FILE>=<DEBUG>`: Debug info of a WebAssembly `--file`: the file before stripping, with its name section and DWARF, or a wasm source map (`.wasm.map`). Without it, `x.debug.wasm` and `x.wasm.map` next to `x.wasm` are used.

## What's in it
The report covers the whole compressed stream: the main JavaScript and every `--file`, in the order they are compressed.

- `report.json`: a summary for analysis by people, scripts or an LLM. It has the totals, the layout of the output file, the cost of every input file, the cost by token kind, the most expensive places (hotspots, identifiers, numbers, repeated strings), suggestions with what to look at next, and the model config. Its `about` entry explains how to read it.
- `report.html`: the input colored by the cost of each byte. Hover over a byte for details, click a suggestion to show it. Collapse files, dim cheap bytes, search with `/`, and jump with the strip on the right. With a source map, the Sources view shows the original files colored by cost.
- `data.bin`: what the model predicted for every bit, read by `rootsqz report`.
- `AGENTS.md`: a guide to this report for coding agents.

The cost of a byte is the number of bits the compressor spends on it, so the costs add up to the size of the compressed stream. A byte costs less when similar bytes came before it, also in earlier files.

`rootsqz report <dir>` prints a view of a report as tab separated rows, so that you or an agent can look at exactly the part that matters:
```sh
rootsqz report out --view hotspots --top 20                # the most expensive 32 byte windows
rootsqz report out --view token-totals --kind ident --top 30
rootsqz report out --view tokens --range 1200..1400       # byte offsets in the decompressed stream
rootsqz report out --view lines --section shader.glsl --sort bits
rootsqz report out --view bits --range 1200..1201 --models
```
The views are `summary`, `sections`, `lines`, `blocks`, `hotspots`, `tokens`, `token-totals`, `bytes`, `bits`, `repeats`, `models`, `sources`, `source-lines`, `wasm-sections`, `wasm-functions`, `wasm-instructions`, `wasm-opcode-totals` and `suggestions`; `rootsqz report --help` lists the filters. `--report-detail` writes the same views as files at compress time.

With `--report-source-map`, the report tells which original source files and lines the bytes of a bundled main JavaScript come from (`--view sources`, `--view source-lines`, `--source`). The [plugin](../README.md#vite--rollup-plugin) passes the source map and the module names of the imported files by itself.

WebAssembly files given with `--file` are parsed: the views `wasm-sections`, `wasm-functions`, `wasm-instructions` and `wasm-opcode-totals` tell what each section, function, data segment and opcode costs, and `hotspots` and `bytes` name the function and instruction of each row. The suggestions point out custom sections of compilers and tools left in the file, LEB128 numbers that are longer than needed (wasm-ld writes them so; wasm-opt fixes it), and the most expensive functions. The report names functions from a name section, and maps them to source lines with DWARF line info (e.g. from `-g`) or a wasm source map. Since you would ship the stripped file, give it the file before stripping with `--report-wasm-debug x.wasm=x.debug.wasm`, or put `x.debug.wasm` or `x.wasm.map` next to `x.wasm`. Functions whose code differs from the debug file get no names or lines. `report.html` has a Wasm tab.

With `--report-models`, the report also tells what each submodel of the mixer alone would have cost, for every byte and in total (`--view models`, `--models`). This helps to tune a `--config`. It needs 32 bytes of memory and disk per input byte and submodel.
