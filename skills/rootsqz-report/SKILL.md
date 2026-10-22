---
name: rootsqz-report
description: Find out where the bytes of a rootsqz-compressed intro (4k/64k demo, one self-extracting HTML file) go, and what to change to make it smaller. Use when asked to shrink the output of rootsqz or of its Vite/Rollup plugin, to explain its size, or to analyze a rootsqz compression report (report.json, `rootsqz report`).
---

# Analyzing the size of a rootsqz intro

rootsqz compresses the main JavaScript and the files packed with it into one stream, with a context-mixing model that predicts every bit. Its compression report tells what each byte of that stream costs, so you can find the expensive parts and change the source where it pays off. Recording the report does not change the output.

## 1. Generate the report

Vite / Rollup plugin (`@r00tkids/rollup-plugin-rootsqz`): set `report: true` in the plugin options of the build config and run the production build, e.g. `npx vite build`. The report is written to `<build.outDir>/rootsqz-output/report/`, e.g. `dist/rootsqz-output/report/`. The plugin also passes the source map of the bundle, so the report maps the bundle back to the original source files; it turns on `build.sourcemap: "hidden"` for that if no source map is set.

```js
rootsqz({ sizeProfile: "64k", report: true })
```

Without changing the config: set the environment variable `ROOTSQZ_REPORT=1` for the build, e.g. `ROOTSQZ_REPORT=1 npx vite build`. The plugin then writes the report as with `report: true`.

Command line: add `--report` to the `rootsqz` command; the report is written to `report/` in the output directory (`-o`). Add `--report-source-map <bundle.js.map>` if the main JavaScript is a bundle with a source map.

```sh
rootsqz -j main.js -f shader.glsl -o out --report --report-source-map main.js.map
```

Node API (`@r00tkids/rootsqz`): the options `report`, `reportSourceMap`, `reportLabels`, `reportWasmDebug`, `reportModels` and `reportDetail`.

WebAssembly files: the report parses them by itself. To name their functions and map them to source lines, it needs debug info, which the shipped file usually has not: put the file before stripping next to it as `x.debug.wasm`, or a wasm source map as `x.wasm.map`, or name it with `reportWasmDebug: { "src/x.wasm": "build/x.full.wasm" }` / `--report-wasm-debug x.wasm=x.full.wasm`. To get one, build the wasm again without stripping, with `-g` for DWARF line info (leave out `wasm-strip` / `--strip-debug`). With line info, the `sources` and `source-lines` views and `--source` cover the wasm code too.

More options (plugin option / command line option):
- `reportModels` / `--report-models`: also record what every submodel of the model predicted, to compare and tune a model config (`--config`). It needs 32 bytes of memory and disk per input byte and submodel.
- `reportDetail` / `--report-detail <VIEW,...>`: also write views as TSV files in the report directory.

The report cannot be combined with the Brotli mode (`brotli` / `--brotli`).

Every build overwrites the report. Copy `report.json` before you change something, to compare after.

## 2. Read the summary

Start with `report.json` in the report directory. Its `about` entry explains the fields. The main ones:
- `totals`: input bytes, `cost_bytes` (the size of the compressed stream from the costs), `output_bytes` (the size of the output file).
- `outputs`: the parts of the output file: the HTML header, the decompressor, the compressed stream, pre-compressed files.
- `sections`: the input files in the order they are compressed: the main JavaScript first, then the bytes of its long base64 strings if it has any (kind `base64`; the main JavaScript has one byte in place of each string), then each packed file. Each has its cost and bits per byte.
- `sources` and `top.source_lines` (with a source map): what the code of each original source file and line costs.
- `suggestions`: candidates worth looking into, each with a `query` for `rootsqz report`.
- `top`: the most expensive hotspots, identifiers, numbers and repeated strings, and blocks of binary files.
- `models` (with `reportModels`): what each submodel alone would have cost.
- `wasm` (with WebAssembly files): per wasm file its sections, most expensive functions and data segments, opcodes, and padded LEB128 numbers; `errors` lists what could not be decoded. `wasm_debug` tells where names and source lines came from.

Do not read `report.html` (a viewer for people that embeds all the data), `data.bin` or `sources.json` (raw data for `rootsqz report`).

## 3. Look closer with `rootsqz report`

```sh
npx rootsqz report <report directory> --view <view> [filters]
```

Use `npx rootsqz` in a project that has the npm package, or `rootsqz` if it is installed. The directory can also be the output directory that contains `report/`. It prints tab separated rows with a header line. Keep the output short with `--top N`; the `bytes` and `bits` views have a row per byte or bit, so use them only with `--range`.

```sh
rootsqz report dist/rootsqz-output --view sources --top 20               # cost per original source file
rootsqz report dist/rootsqz-output --view source-lines --source src/scene.ts --top 30
rootsqz report dist/rootsqz-output --view hotspots --top 20             # the most expensive 32 byte windows
rootsqz report dist/rootsqz-output --view token-totals --kind number --top 30
rootsqz report dist/rootsqz-output --view repeats --top 20              # strings that came before and still cost
rootsqz report dist/rootsqz-output --view sections
rootsqz report dist/rootsqz-output --view tokens --range 1200..1400     # what is at these offsets
rootsqz report dist/rootsqz-output --view bits --range 1200..1201 --models
rootsqz report dist/rootsqz-output --view wasm-functions --top 20       # wasm functions and data segments
rootsqz report dist/rootsqz-output --view wasm-instructions --range 3400..3500
rootsqz report dist/rootsqz-output --view wasm-opcode-totals --top 20
```

`rootsqz report --help` lists all views and filters (`--section`, `--source`, `--range`, `--kind`, `--sort`, `--top`, `--min-bits`, `--models`). Offsets are byte offsets in the decompressed stream, which is the main JavaScript, the decoded base64 strings and each packed file. The `source` and `source_line` columns name the original file and line of the first byte of a row.

## 4. Change the source, then measure

How to read the costs:
- The cost of a byte is the number of bits the compressor spends on it. The costs add up to the size of the compressed stream.
- A byte costs less when similar bytes came before it, also in earlier files. So a change also changes the cost of what comes after it, and the order of the files matters.
- The suggestions and estimates are heuristics. Rebuild after every change and compare `totals.output_bytes` with the copy from before. Keep a change only if it saves bytes.

Changes that often pay off:
- Change the original source, not the bundle. Find the source file and line of an expensive part with a source map (`sources`, `source-lines`, the `source` column).
- Data that does not look like code, e.g. base64 strings, long arrays of numbers or tables, costs much in the JavaScript. Pack it as a binary file instead (the plugin's `?rootsqz-bin` import, `--file` on the command line), in a compact encoding, or generate it in code.
- Number literals with many digits cost bits for each digit. Use fewer digits where the precision does not show.
- A string that occurred before is cheap, but its first occurrence is not. Write similar code the same way, e.g. the same names and the same order of arguments and fields.
- A packed file that costs more than 8 bits per byte is already compressed: pack it as it is (`&pre-compressed` with the plugin, `--pre-compressed-file` on the command line).
- `sizeProfile: "64k-semi-fast"` and `"64k-fast"` trade size for decoding speed: `"64k"` packs about 2% and 6% smaller. Suggest it only if the user can accept the slower decode.
- WebAssembly: for a large module, try `sizeProfile: "64k-wasm"` (`--size-profile 64k-wasm`), which models wasm; a small one packs smaller without. Strip custom sections of tools (`name`, `producers`, `.debug_*`) but keep those the intro reads with `WebAssembly.Module.customSections`. Optimize with `wasm-opt -Oz`, which also writes LEB128 numbers without padding. Look at expensive functions with `wasm-instructions`; constants (`F32Const`, `I32Const`) and large data segments are often the place to start.
- With `reportModels`, `models` and the `--models` columns show which submodels help, to tune a custom model config.
