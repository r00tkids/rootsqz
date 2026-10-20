# Changelog

All notable changes to rootsqz and its npm packages. The crate and all npm packages share one version.

Changes that are not released yet are listed under Unreleased. On a release, that section gets the version and the date. The release workflow uses the section of a version as the notes of its GitHub release.

## [Unreleased]

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
