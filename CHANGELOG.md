# Changelog

All notable changes to rootsqz and its npm packages. The crate and all npm packages share one version.

Changes that are not released yet are listed under Unreleased. On a release, that section gets the version and the date. The release workflow uses the section of a version as the notes of its GitHub release.

## [Unreleased]

## [0.15.0] - 2026-10-22

### Added
- `--size-profile 64k-semi-fast` and `64k-fast` (`sizeProfile` in the Node API and the plugin), 64k presets that mix fewer models, so they decode faster and with less memory, but pack larger. On three 64k intros, decoder included, with decode times in Chrome:
  - `64k`: 130.9 KB, 8–13 s, up to 1 GB of memory (25 models, 7 weight sets, 5 APMs)
  - `64k-semi-fast`: 133.9 KB (+2.3%), 5–7 s, 0.6–0.7 GB (15 models, 5 weight sets, 2 APMs)
  - `64k-fast`: 138.3 KB (+5.7%), 3–6 s, 0.5–0.6 GB (11 models, 3 weight sets, 2 APMs)

  Both still pack each of the three smaller than mashi 0.1.7 (143.6 KB in total).

## [0.14.0] - 2026-10-22

### Added
- `--title <TEXT>` sets the page title of the web output before it decompresses, and `--boot-js <FILE>` runs a script first, before the loader and decompression. The Node API and the plugin have them as `title` and `bootJs`. Outputs without them are unchanged.
- The plugin writes the compression report when the environment variable `ROOTSQZ_REPORT` is set (e.g. `ROOTSQZ_REPORT=1 vite build`), without a change to the build config. `0` and `false` leave it off.

## [0.13.0] - 2026-10-21

### Changed
- The `64k` and `64k-wasm` presets pack about 1.2% smaller, decoder included (1.6 KB on three 64k intros), with settings found by `rootsqz search`: more CharClass models, Match models that compare more candidates, ContextMixer weights chosen by the state of more models, and lower learning rates. The decoder still needs up to about 1 GB of memory.

## [0.12.0] - 2026-10-21

### Changed
- `--search` saves the best config so far as `search_config.json` after each improvement, so a stopped search leaves a config to build with, and running the same command again resumes it. It keeps its state in the output directory, or in the `--search-cache` directory if given, and removes it when the search is done.
- **Breaking:** `--size-profile` (and `sizeProfile` of the Node API and the plugin) defaults to `64k` instead of `4k`, as rootsqz is made for 64k intros. Its decoder needs up to about 1 GB of memory. For a 4k intro, pass `--size-profile 4k`. `--brotli` without `--size-profile` still works.

## [0.11.0] - 2026-10-21

### Added
- `--search` searches for the model config that packs the input smallest before packing, counting the bytes each change adds to or takes out of the decoder, and writes the config it found to `search_config.json` in the output directory. It can drop a model type whose decoder code does not pay for itself on the input, and tries the context models of WebAssembly files of the `64k-wasm` preset, and tunes them. `--search-threads` and `--search-cache` set how many configs it tries at once and where it keeps the result for the next build. `rootsqz search` runs the search alone, also for one config over several demos. The Node API and the plugin have the options `search`, `searchThreads` and `searchCache`.
- The config option `static_model_params.count_offset` (default `0.2`) sets how fast the counters of the context models learn: each moves by 1 / (count + `count_offset`) towards the bit.
- The model type `Match` predicts the byte that followed an earlier occurrence of the last `min_len` bytes anywhere in the input, learns how far to trust it by the length of the match, and keeps following the match after a byte that differs. `candidates` sets how many earlier occurrences it compares, `expected_byte_context` learns the trust per predicted byte, and `second` follows the most recent occurrence that predicts another byte than the most recent one.
- The `64k` preset models the tokens of the JavaScript: contexts from the bracket, object key and punctuation around a token, the token that followed the previous token before, the bracket nesting, and the sequence of token classes. JavaScript with object literals and scene data packs up to about 2.6 KB smaller; the decoder is about 420 bytes larger. A config can use them as `{"type":"JsContext","variant":0}` to `3`.
- Base64 strings of at least 64 characters in the main JavaScript, such as fonts or wasm modules in data URLs, are compressed as the bytes they encode, and the decoder puts them back with `btoa`. On 64k intros this saves 0.1 to 0.9 KB. In the report, the decoded bytes are a section of kind `base64` after the main JavaScript, which has one byte in place of each string.
- `--size-profile 64k-wasm`, the `64k` preset with context models for WebAssembly files given with `--file`. They follow the structure of the module: the section, the opcode and its operand, the previous opcodes and the function, with indexes such as those of the exports predicted from the previous one. With a 90 KB wasm module the output is about 0.5 KB smaller than with `64k`, the decoder included; with a 7 KB one it is about 0.45 KB larger, so use `64k` for small modules. A config gets them with `static_model_params.wasm.models`, as in [the 64k-wasm preset](src/compressor/size_64k_wasm_config.json). The Node API and the plugin take `sizeProfile: "64k-wasm"`.
- Binary files given with `--file` get models for fixed-size records, such as tables of int16 or float32 values: they predict a byte from the same column of the last records, from bytes 4 and 8 back, and words from the last two records. The compressor finds the record size of each 512-byte block of the files, tries the models, and keeps them only for the blocks where they save more than they add to the decoder, so outputs where they don't win are unchanged. A config sets them in `static_model_params.records.models`. A 64k intro with tables of numbers in its files packed about 2.2 KB smaller, its decoder included.
- `static_model_params.context_input_scale` (default `1`) multiplies the mixer inputs of the context models.
- The model type `ContextMixer` mixes its models paq8-style: it has a weight set per selector (`None`, `Order1` for the previous byte, `Known` for how many models know their context, `{"State": i}` for the state of model `i`, such as the length of a match, and `{"Context": i}` for the context that model `i` offers, which for a `JsContext` model is the bracket, token class and punctuation and for other models 0), each picking its weights by its context, and mixes their outputs with a final weight set. `apms` refine its probability with adaptive probability maps by the previous bytes, by the state of a model, such as the length of a match, or by the context a model offers, and average it with theirs. `Match` models give it their match length as a second input. It takes all the inputs of each model, and the count of its context as its state. `rootsqz search` also searches the learning rates of a `ContextMixer`.

- `--search` and `rootsqz search` also try `context_input_scale`, the table size (`hash_table_pow2_size`), adding and removing `ContextMixer` selectors and APMs, the limit, weight and hash size of each APM, `CharClass` orders 7 and 8, and changes to the record models. They skip configs whose decoder would need more than 1 GB of memory, by an estimate that `rootsqz search --eval` prints; `rootsqz search --max-memory-mb` changes the limit. `rootsqz search --state <FILE>` lets a stopped search resume where it was.

### Changed
- The context models give the mixer five inputs per context instead of one: the probability of the context, also on a linear scale, the probability learnt for its bit history (the last 7 bits seen in it), the next bit of the last byte seen in the context of the byte weighted by how many times in a row it came, and whether the context is new. Outputs are about 3.5% smaller (4.8 KB on three 64k intros); the decoder is about 250 bytes larger.
- `--size-profile 64k` lets the counters of its byte and JavaScript contexts count up to 255 instead of 15, so they settle on long-run probabilities beside the bit history: about 0.25% smaller.
- Contexts no longer share their counters when they fall on the same place in the table of the context models: each place keeps a check of its context, and a different context starts afresh there. Outputs are about 0.8% smaller (1.1 KB on three 64k intros). The table of the decoder takes 8 bytes per entry instead of 4, so 512 MB with the default `hash_table_pow2_size` of 26.
- The decoder is about 450 to 510 bytes smaller after deflate with `--size-profile 64k`: it hashes with 32-bit integer arithmetic instead of BigInt, keeps the counters in a typed array, and includes only the code of the model types the config uses. The word model hashes words with 32-bit FNV-1a, which packs about the same, so the output of the same input differs from before.
- `--size-profile 64k` packs about 2% smaller, as it mixes with `ContextMixer`, with weights selected by the JavaScript tokens and the match length, and refines the probability with six adaptive probability maps. The decoder is about 590 bytes larger, and needs less memory: about 1 GB at most, as the counter table has 2^24 slots instead of 2^26.
- `--size-profile 64k` packs about 5% smaller: it uses six `Match` models instead of `MatchPredictor`, whose history was 64 KiB.

### Fixed
- The output could decode wrongly: the decoder computed some predictions slightly differently from the compressor, depending on the browser or Node version, and from that point on decoded garbage. With `--size-profile 64k`, a 365 KB script decoded wrongly from byte 125497 on. The compressor and the decoder now compute every prediction exactly the same in every JavaScript engine. The decoder is about 90 bytes bigger.
- A config with a `hash_table_pow2_size` other than 26 decoded wrongly, as the decoder always used a table of 2^26 counters.
- A config with the model type `AdaptiveProbabilityMap` packed, but its decoder threw a `ReferenceError`, as the decoder has no such model. rootsqz now rejects it; use the `apms` of a `ContextMixer` instead.

## [0.10.0] - 2026-10-20

### Added
- `--decoder <serial|sliced|worker>` chooses how the web output decodes: in one go on the main thread, in slices on the main thread so the page repaints, or in a web worker so the page stays responsive while it decodes. The worker reports progress to a loader script like the sliced decoder. Without the option, the output is the same as before. The Node API and the plugin have the option `decoder`, and the Node API exports the type `Decoder`.

### Fixed
- The report reads WebAssembly files with SIMD and relaxed SIMD instructions. Before, it did not recognize such a file as wasm at all, so it showed no functions or names. An instruction that the report cannot decode now loses only the rest of its function, which shows as `(not decoded)`, and `wasm[].errors` in `report.json` lists it.

## [0.9.1] - 2026-10-20

### Fixed
- The suggestion about bytes that the source map does not cover, e.g. shaders that a plugin minified, tells where they start, instead of a query for a source "(not mapped)" that does not work.

## [0.9.0] - 2026-10-20

### Added
- The report parses WebAssembly files given with `--file` or imported with the plugin. The views `wasm-sections`, `wasm-functions`, `wasm-instructions` and `wasm-opcode-totals` show what each section, function, data segment and opcode costs, and `hotspots` and `bytes` name the function and instruction of each row. Suggestions point out custom sections of tools left in the file, padded LEB128 numbers, and the most expensive functions. `report.html` has a Wasm tab and names the function and instruction of each byte.
- `--report-wasm-debug <FILE>=<DEBUG>` gives the report the debug info of a wasm file: the file before stripping, for function names and DWARF source lines, or a wasm source map (`.wasm.map`). Without it, the report uses `x.debug.wasm` and `x.wasm.map` next to `x.wasm`, and a name section or DWARF left in the file. Functions whose code differs from the debug file get no names. The Node API and the plugin have the option `reportWasmDebug`; the plugin also looks next to the imported files.

### Changed
- The views `sources` and `source-lines`, `--source` and the Sources view of `report.html` cover every mapped file, not only the main JavaScript. Reports written by earlier versions cannot be read; compress again.

### Fixed
- `report.html` counts bytes at the start of the stream that no source map covers as "(not mapped)".

## [0.8.0] - 2026-10-20

### Added
- `--report` writes a compression report for analysis, also by an LLM: `report.json` with totals, the layout of the output, the cost of every input file, hotspots, expensive identifiers, numbers and repeated strings, and suggestions. It covers the files given with `--file` as well.
- `rootsqz install-skill [DIR]` installs the agent skill `rootsqz-report` into a project (`.claude/skills` and `.agents/skills`). It teaches coding agents to make a compression report, read it, and check that a change saves bytes. Each report directory also gets an `AGENTS.md` (and a `CLAUDE.md` that imports it) about that report.
- `rootsqz report <dir>` prints views of a report as tab separated rows: per section, line, block, hotspot, token, byte or bit, with filters by section, byte range, token kind and cost.
- `--report-models` records what every submodel predicted, to compare the models of a config.
- `--report-detail <VIEW,...>` writes views of the report as TSV files at compress time.
- The Node API and the plugin have the options `reportModels` and `reportDetail`. The Node API exports the type `ReportView`.
- `--report-source-map <FILE>` maps the costs of the main JavaScript back to its original source files and lines, with the views `sources` and `source-lines`. It also maps the lines of multi-line strings, such as shaders in template literals, when they are in the source as they are or minified. `--report-label <FILE>=<LABEL>` names a file in the report. The Node API has the options `reportSourceMap` and `reportLabels`.
- With `report`, the plugin passes the source map of the bundle and names the imported files in the report by their module and import query, marked "(transformed)" when one of the `fileTransforms` made them. It turns on `sourcemap: "hidden"` for that if no source map is set.

### Changed
- **Breaking:** `--report` writes to `report/` in the output directory instead of `report.html` next to the output. `report.html` shows the cost of every byte in bits, keeps line breaks, covers the `--file` files, and has the summary, the suggestions and details per byte. It stays fast with 64k intros. Files collapse, cheap bytes can be dimmed, and you can search the input. A strip on the side shows the whole input. Binary files show as hex. With a source map, it shows the original sources colored by cost, lists them by folder, and can show only the bytes of one of them.

### Fixed
- `rootsqz web …` works. It failed because the options were also required before the subcommand.

## [0.7.0] - 2026-10-20

### Added
- `--loader progress` (`loader: { preset: "progress" }`), a loader with the browser's own progress bar, a `<progress>` element. It is the smallest preset with progress.

### Changed
- Every web output is about 180 bytes smaller: the names in the boot code are now minified, it is minified in 3 passes, and deflated with zopfli instead of zlib. The main JavaScript now runs in the global scope, like a script, instead of in a function of the boot code.
- The loaders are smaller: in the ray tracer test, `--loader bar` adds 271 bytes instead of 369, and `--loader spinner` 195 instead of 221. Outputs with a loader script no longer also contain the synchronous decoder, and leading and trailing whitespace of `--loader-html` is no longer packed.
- `rsqz.progress` is first called after the first slice of decoding, no longer with 0 before it starts.

## [0.6.0] - 2026-10-20

### Added
- A loader for the web output, shown while it decompresses: `--loader spinner` or `--loader bar`, or your own `--loader-html <file>` and `--loader-js <file>`. The script gets the progress through `rsqz.progress = (done, total) => ...`. `--loader-keep` keeps the loader when the main JavaScript runs, so the demo can continue the progress bar. Outputs without a loader don't change.
- The `loader` option of `compress` and of the plugin, e.g. `loader: { preset: "bar" }` or `loader: { html: "loader.html", js: "loader.js", keep: true }`, with the type `LoaderOptions`.

### Changed
- The generated output starts about 0.6 seconds sooner: setting up the decompression model took about 650 ms and now takes about 50 ms. The output grows by 1 byte.

## [0.5.1] - 2026-10-19

### Added
- CHANGELOG.md, whose section of a version is the notes of its GitHub release.

## [0.5.0] - 2026-10-19

### Changed
- **Breaking:** The command line options `--files` and `--pre-compressed-files` are renamed to `--file` and `--pre-compressed-file`. Give the option once per file; values are no longer split at commas, so a path can contain a comma. The short forms `-f` and `-p` stay.
- **Breaking:** The plugin import query parameter `&compressed` is renamed to `&pre-compressed`, as in `logo.jpg?rootsqz-bin&pre-compressed`. The old name fails the build with a message.

### Added
- The options type of the plugin is exported as `RootsqzOptions`.
- JSDoc comments on all public interfaces of `@r00tkids/rootsqz` and `@r00tkids/rollup-plugin-rootsqz`.
- The READMEs document the Vite / Rollup plugin: asset imports, JSON imports, the packing limits and recommended build options.
- The Vite example imports a text file and a JSON file as a string, and uses a hidden source map.

## [0.4.3] - 2026-10-19

### Fixed
- The release workflow checks npm trusted publishing for every package before it publishes anything.

## [0.4.2] - 2026-10-16

Not published: the release workflow failed before it published to npm.

### Fixed
- The plugin ignores imported files that are removed from the bundle by tree shaking.

### Changed
- The release workflow publishes to npm through trusted publishing only, and creates the GitHub release after the npm packages.

## [0.4.1] - 2026-10-07

### Changed
- **Breaking:** websqz is renamed to rootsqz: the crate, the executable and the runtime global, which is now `rsqz` instead of `wsqz`.
- The Rollup / Vite plugin moved into this repository as `@r00tkids/rollup-plugin-rootsqz`.

### Added
- npm package `@r00tkids/rootsqz` with the `rootsqz` command, a Node API and prebuilt executables for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64).
- `--size-profile 64k`, a preset for larger JavaScript inputs with a bounded match predictor.
- `--config` for a complete JSON compression config, and the optional models `CharClass`, `TokenPosition`, `MatchPredictor` and `IndirectByte`.
- `--brotli`, which packs the output as a Brotli stream decoded with `DecompressionStream('brotli')`.

## [0.4] - 2026-01-28

### Fixed
- The content of extra files no longer breaks the encoder stream.

## [0.3] - 2026-01-23

### Changed
- Updated dependencies.

## [0.2] - 2025-12-21

### Fixed
- Compressing several extra files.

## [0.1] - 2025-11-16

First release.

[0.15.0]: https://github.com/r00tkids/rootsqz/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/r00tkids/rootsqz/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/r00tkids/rootsqz/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/r00tkids/rootsqz/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/r00tkids/rootsqz/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/r00tkids/rootsqz/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/r00tkids/rootsqz/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/r00tkids/rootsqz/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/r00tkids/rootsqz/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/r00tkids/rootsqz/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/r00tkids/rootsqz/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/r00tkids/rootsqz/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/r00tkids/rootsqz/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/r00tkids/rootsqz/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/r00tkids/rootsqz/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/r00tkids/rootsqz/compare/v0.4...v0.4.1
[0.4]: https://github.com/r00tkids/rootsqz/compare/v0.3...v0.4
[0.3]: https://github.com/r00tkids/rootsqz/compare/v0.2...v0.3
[0.2]: https://github.com/r00tkids/rootsqz/compare/v0.1...v0.2
[0.1]: https://github.com/r00tkids/rootsqz/releases/tag/v0.1
