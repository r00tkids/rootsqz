# rootsqz
rootsqz is a tool for compressing and decompressing demo intros for the web. The current overhead is about 1.6 KiB, so it's primarily intended for 64KiB intros, though this may change in the future. 
It's inspired by [Crinkler](https://github.com/runestubbe/Crinkler) and ZPaq series of compressors.

There is also a [Vite / Rollup plugin](#vite--rollup-plugin).

## Features
- High compression ratio for JavaScript and binary assets - about 20% better than deflate-raw
- Extensible for new compression strategies

## Installation

### From npm
```sh
npm install -D @r00tkids/rootsqz
npx rootsqz --help
```
This installs a prebuilt executable for macOS (arm64, x64), Linux (arm64, x64) or Windows (x64), and the UglifyJS it needs. The package also has a Node API, see [packages/rootsqz](packages/rootsqz).

### From source
1. Clone the repository:
   ```sh
   git clone https://github.com/r00tkids/rootsqz.git
   cd rootsqz
   ```
2. Install dependencies:
   - Install [UglifyJS](https://github.com/mishoo/UglifyJS):
     ```sh
     npm install -g uglify-js
     ```
   - Build and install rootsqz (requires Rust and Cargo):
     ```sh
     cargo install --path .
     ```

## Usage

Basic compression example:
```sh
rootsqz --js-main examples/cli/index.js -f examples/cli/bundled.glsl --output-directory out
```

Pass `--brotli` to append a Brotli stream to the single `index.html` for web
inputs of any size.
The browser decodes it with `DecompressionStream('brotli')`. This works in
Firefox 147+ and Safari 18.4+. As of September 2026, Chrome does not support
Brotli in `DecompressionStream`, so the arithmetic compressor remains the
default for browser compatibility.

Long base64 strings in the main JavaScript, such as fonts or wasm modules in data URLs, are compressed as the bytes they encode, and the decoder encodes them again with `btoa`.

Binary files given with `--file` get models for fixed-size records, such as tables of numbers, in the parts of the files where a trial compression shows that they pay for their decoder code.

Options:
- `--js-main <file>`: Entry point JavaScript file
- `--file <FILE>`, `-f`: Extra file to be compressed. Repeat the option for each file. Order matters, so files of similar content should be ordered together.
- `--pre-compressed-file <FILE>`, `-p`: Extra file that is already compressed (jpeg, mp4 etc.) and is stored as it is. Repeat the option for each file.
- `--output-directory <dir>`: Output directory for compressed files
- `--size-profile <4k|64k>`: Embedded compression preset (default: `4k`). The `64k` preset adds `Match` models that find repeats anywhere in larger inputs, contexts from the tokens, brackets and object keys of the JavaScript, and a `ContextMixer`; its decoder needs up to about 1 GB of memory.
- `--brotli`: Use native Brotli decoding for web inputs (Firefox 147+ / Safari 18.4+).
- `--config <file.json>`: Use a complete JSON `CompressConfig` instead of the embedded preset. This takes precedence over `--size-profile`.
- `--loader <spinner|bar|progress>`, `--loader-html <file>`, `--loader-js <file>`, `--loader-keep`: Show a loader while the web output decompresses, see [Loader](#loader).
- `--decoder <serial|sliced|worker>`: How the web output decodes, see [Decoder](#decoder).
- `--search`: Search for the model config that packs this input smallest, starting from `--config` or the `--size-profile` preset, see [Config search](#config-search). `--search-threads <N>` sets how many configs it tries at the same time, and `--search-cache <DIR>` keeps the config it found, to reuse it when the same input is packed again.
- `--report`, `-r`: Write a [compression report](#compression-report) to `report/` in the output directory.
- `--report-models`: Also record what every submodel predicted (implies `--report`).
- `--report-detail <VIEW,...>`: Also write these views of the report as TSV files, e.g. `lines,tokens` (implies `--report`).
- `--report-source-map <FILE>`: Source map of the main JavaScript, to map its costs back to the original source files and lines (implies `--report`).
- `--report-label <FILE>=<LABEL>`: Name of a file in the report, e.g. the module it was made from. Repeat the option for each file (implies `--report`).
- `--report-wasm-debug <FILE>=<DEBUG>`: Debug info of a WebAssembly `--file`: the file before stripping, with its name section and DWARF, or a wasm source map (`.wasm.map`). Without it, `x.debug.wasm` and `x.wasm.map` next to `x.wasm` are used (implies `--report`).
- See `rootsqz --help` for more CLI options

Both presets are available for `web` and `node` targets. To customize a model, copy [the 4k preset](src/compressor/default_config.json) or [the 64k preset](src/compressor/size_64k_config.json), edit its `model.models` array, and pass it with `--config`. Supported optional model entries include `{"type":"CharClass","order":3}`, `{"type":"TokenPosition"}`, `{"type":"MatchPredictor","context_bytes":2,"confidence":0.97,"table_bits":16}`, `{"type":"Match","min_len":6}` (also `candidates`, `expected_byte_context` and `second`), `{"type":"IndirectByte","context_bytes":2,"max_count":15}`, and `{"type":"JsContext","variant":0}` (variants 0 to 3: the bracket, object key and punctuation around a token, the token that followed the previous token before, the last byte and the bracket nesting, and the sequence of token classes and punctuation). The decoder is generated from the selected config. `static_model_params.count_offset` (default `0.2`) sets how fast the counters of the context models learn: each moves by 1 / (count + `count_offset`) towards the bit.

The 64k preset mixes its models with a `ContextMixer`, paq8-style. Its `selectors` each choose a set of weights by a context: `"None"` (one set), `"Order1"` (the previous byte), `"Known"` (how many models know their context), `{"State": i}` (the state of model `i` of `models`, such as the expected bit and length of a `Match`) and `{"Context": i}` (the context that model `i` offers: a `JsContext` offers the bracket, token class and punctuation, other models 0). A final weight set mixes their outputs. Its `apms` refine the result by `"Order0"`, `"Order1"`, `"Order2"` (hashed to `bits` bits), `{"State": i}` or `{"Context": i}`, and their probabilities are averaged with the mixer's by their `weight`. It takes all the inputs of each model; `static_model_params.context_input_scale` (default `1`) multiplies those of the context models, which the ContextMixer wants larger than the `Mixer`. When you add or remove models of a `ContextMixer`, keep the indexes `i` in its selectors and APMs pointing at the right models.

### Config search
`--search` tries changes to the config one at a time: a changed context mask, count limit, match length, learning rate, `context_input_scale` or table size, a model added or removed, all models of a type removed, a `ContextMixer` selector or APM added or removed or an APM's limit, weight or hash size changed, and for WebAssembly files and record ranges, changes to their models. It skips changes after which the decoder would need more than 1 GB of memory, by an estimate. It keeps a change that makes the output smaller, counting the bytes that the change adds to or takes out of the decoder, until no change helps. So a model type that does not pay for its code in the decoder on an input is left out of its output. It takes about an hour for a 64k intro. The config it found is written to `search_config.json` in the output directory; pass it with `--config` to pack again without searching. The result does not depend on `--search-threads`.

`rootsqz search -i main.js,data.bin -o config.json` runs the same search without packing, writing the best config so far after each improvement. Give `-i` once per demo (the main JavaScript, then its files, separated by commas) to find one config for all of them; `--eval` prints the size of each demo with the `--config` given, and the estimated memory of its decoder. `--max-memory-mb` sets the memory limit (default 1000). With `--state <FILE>` it keeps the changes it tried and the sizes it packed in that file; to resume a stopped search, run it again with the same `--state` and `--config` set to its `-o` file.

## Compression report
`--report` writes a report of where the bytes go to `report/` in the output directory. It covers the whole compressed stream: the main JavaScript and every `--file`, in the order they are compressed.

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

With `--report-source-map`, the report tells which original source files and lines the bytes of a bundled main JavaScript come from (`--view sources`, `--view source-lines`, `--source`). The [plugin](#vite--rollup-plugin) passes the source map and the module names of the imported files by itself.

WebAssembly files given with `--file` are parsed: the views `wasm-sections`, `wasm-functions`, `wasm-instructions` and `wasm-opcode-totals` tell what each section, function, data segment and opcode costs, and `hotspots` and `bytes` name the function and instruction of each row. The suggestions point out custom sections of compilers and tools left in the file, LEB128 numbers that are longer than needed (wasm-ld writes them so; wasm-opt fixes it), and the most expensive functions. The report names functions from a name section, and maps them to source lines with DWARF line info (e.g. from `-g`) or a wasm source map. Since you would ship the stripped file, give it the file before stripping with `--report-wasm-debug x.wasm=x.debug.wasm`, or put `x.debug.wasm` or `x.wasm.map` next to `x.wasm`. Functions whose code differs from the debug file get no names or lines. `report.html` has a Wasm tab.

With `--report-models`, the report also tells what each submodel of the mixer alone would have cost, for every byte and in total (`--view models`, `--models`). This helps to tune a `--config`. It needs 32 bytes of memory and disk per input byte and submodel.

### For coding agents
Coding agents such as Claude Code or Codex can make and read the report themselves. Install the agent skill into your project:
```sh
npx rootsqz install-skill    # writes .claude/skills/rootsqz-report/SKILL.md and .agents/skills/rootsqz-report/SKILL.md
```
When you ask the agent to make the intro smaller, it learns from the skill how to turn on the report, read `report.json`, query it with `rootsqz report`, and check that a change saves bytes. Commit the skill with your project, and install it again after updating rootsqz. The skill is [skills/rootsqz-report/SKILL.md](skills/rootsqz-report/SKILL.md).

Each report directory also has an `AGENTS.md` (and a `CLAUDE.md` that imports it) with what is in that report and how to query it, for agents that open it.

## Runtime API
To access the contents of files specified with `--file` or `--pre-compressed-file`, use `rsqz.files["<FILENAME>"]`.
`rsqz.files["<FILENAME>"]` returns an `Uint8Array`, to read it as text use `new TextDecoder().decode(rsqz.files["example.glsl"])`.

Note: `<FILENAME>` refers to the base name of the file, not its full or relative path.

## Loader
The web output can show a loader while it decompresses. Without loader options, the output is the same as before, not a byte larger.

- `--loader spinner`: A spinning ring. By default, the decoder runs in one go.
- `--loader bar`: A progress bar. By default, the decoder runs in slices.
- `--loader progress`: The browser's own progress bar, a `<progress>` element, so it looks different in each browser. By default, the decoder runs in slices. It is the smallest preset with progress.
- `--loader-html <file>`: Your own markup and CSS, e.g. `<style>…</style><div id=l></div>`. It is packed as it is, without minification, so keep it compact. Scripts in it do not run.
- `--loader-js <file>`: Your own script. It runs after the markup is inserted, so it can use it. Set `rsqz.progress` in it:
  ```js
  rsqz.progress = (done, total) => {
      document.querySelector("#l").style.width = done / total * 100 + "%";
  };
  ```
  It is called with the number of decoded bytes about every 12 ms (16 ms with `--decoder worker`), and with `total` at the end. With `--decoder serial` it is not called while decoding.
- `--loader-keep`: Keep the loader when the main JavaScript runs. By default the page is cleared first. With it, the demo can continue a progress bar during its own precalculation by calling `rsqz.progress` itself, and removes the loader when done.

`--loader` cannot be combined with `--loader-html` or `--loader-js`; to change a preset, copy it from [src/web/templates/web/loaders](src/web/templates/web/loaders). Loaders are only for the `web` target without `--brotli`.

How it works: by default, the decoder blocks the main thread, and the browser paints nothing while it runs. With only `--loader-html`, the decoder runs in one go after the loader is painted once, so only CSS `transform` and `opacity` animations, which browsers run off the main thread, keep moving; a spinner works, a progress bar does not. With `--loader-js`, the decoder yields to the browser between slices, so the page repaints. This makes decoding slightly slower. `--decoder` chooses this yourself, see below.

## Decoder
`--decoder` chooses how the web output decodes:

- `serial`: In one go on the main thread. The smallest; the page does not repaint until it is done. The default without a loader script.
- `sliced`: On the main thread, in slices of about 12 ms, so the page repaints between them and a loader script gets progress. The default with `--loader-js`, `--loader bar` and `--loader progress`.
- `worker`: In a web worker, made from the decoder in the boot code, so the main thread stays free: the page stays responsive, any loader animates smoothly, and a loader script gets progress. It adds about 120 bytes without a loader, and about the same as `sliced` with one.

Without `--decoder`, the output is the same as before. `--decoder` is only for the `web` target without `--brotli`.

## Vite / Rollup plugin
[`@r00tkids/rollup-plugin-rootsqz`](packages/rollup-plugin-rootsqz) runs rootsqz on the bundle, so a Vite or Rollup project builds straight to one compressed HTML file.

```sh
npm install -D @r00tkids/rollup-plugin-rootsqz
```
This also installs the rootsqz executable through `@r00tkids/rootsqz`.

```js
// vite.config.js
import { defineConfig } from 'vite';
import rootsqz from '@r00tkids/rollup-plugin-rootsqz';

export default defineConfig({
  build: {
    // Disable module preloading for better compression results
    modulePreload: false,
  },
  plugins: [rootsqz({ sizeProfile: "64k" })]
});
```

Import assets with a query parameter to have them compressed with the code:
```js
import text from "./assets/scroller.txt?rootsqz-txt";             // string
import data from "./assets/rand.bin?rootsqz-bin";                  // Uint8Array
import image from "./assets/logo.jpg?rootsqz-bin&pre-compressed";  // already compressed, stored as it is
```
The plugin orders the files by type and extension for a better compression ratio. For other file types, e.g. minified shaders, use the `fileTransforms` option.

Import JSON without a query parameter. Vite then inlines it as a JavaScript object, which packs smaller than the file as it is and needs no `JSON.parse`. Only when you need the JSON text as a string, e.g. for a library that parses it itself, import it with `?rootsqz-txt&raw`:
```js
import levelText from "./assets/level.json?rootsqz-txt&raw";  // string, packed as it is, so minify the file
```
`&raw` keeps Vite's own JSON handling away from the import, without it the build fails. The same applies to CSS (`style.css?rootsqz-txt&raw`). See [json-as-string-example.js](examples/vite/src/json-as-string-example.js).

`vite build` writes the result to `rootsqz-output` in the output directory, e.g. `dist/rootsqz-output/index.html`. The dev server does not compress, it inlines the imported files instead.

Notes:
- The bundle must be a single JavaScript file. If the build fails because of several chunks, set `build.rollupOptions.output.inlineDynamicImports` to `true`.
- The plugin only compresses when `NODE_ENV` is `production`. `vite build` sets this by default; with plain Rollup, set it yourself.
- Only the JavaScript bundle and the files imported with `?rootsqz-txt` or `?rootsqz-bin` are packed. Markup in `index.html` and CSS imported with a plain `import "./style.css"` are not in the output. Create the elements from JavaScript, and import CSS as a string to add it yourself:
  ```js
  import css from "./style.css?inline";
  document.head.appendChild(document.createElement("style")).textContent = css;
  document.body.insertAdjacentHTML("beforeend", "<canvas id=c>");
  ```
- Leave `build.sourcemap` off or set it to `'hidden'`. With `true`, the comment that refers to the source map is packed into the output.
- To show a loader while the output decompresses, set `loader: { preset: "spinner" }`, `{ preset: "bar" }` or `{ preset: "progress" }`, or your own `{ html, js }` files. See [Loader](#loader). To keep the page responsive while it decodes, set `decoder: "worker"`, see [Decoder](#decoder).
- `search: true` searches for the model config that packs the bundle smallest before packing, see [Config search](#config-search). It takes about an hour for a 64k intro, so set `searchCache` to a directory that keeps the config found for the next build of the same bundle.
- With `report: true`, the plugin writes a [compression report](#compression-report) to `rootsqz-output/report`. It names the imported files by their module and maps the bundle back to its original sources, for which it turns on `build.sourcemap: 'hidden'` if no source map is set. For an imported `x.wasm`, it gives the report `x.debug.wasm` (the unstripped file) or `x.wasm.map` next to it as debug info, or the file that `reportWasmDebug: { 'src/x.wasm': 'build/x.full.wasm' }` names, so the report names its functions and maps them to source lines. Run `npx rootsqz install-skill` once to teach coding agents to use it ([For coding agents](#for-coding-agents)).

See the [plugin README](packages/rollup-plugin-rootsqz) for all options and [examples/vite](examples/vite) for a working project with `vite-plugin-glsl`.

## Development
The Rust crate is at the repository root. The npm packages are a [pnpm](https://pnpm.io) workspace:

| Path | Package |
|---|---|
| `packages/rootsqz` | `@r00tkids/rootsqz`: `rootsqz` command and Node API, runs the executable |
| `packages/rollup-plugin-rootsqz` | `@r00tkids/rollup-plugin-rootsqz` |
| `npm/<platform>` | `@r00tkids/rootsqz-<platform>`: the executable for one platform |
| `examples/vite` | Example project for the plugin |

```sh
node scripts/build-target.mjs   # build the executable into npm/<platform>/bin
pnpm install
pnpm run build                  # build the packages and the vite example
```

`node scripts/build-target.mjs <target>` builds another target, e.g. `linux-arm64`. The Linux targets need [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild). To try a `cargo build` without copying it, set `ROOTSQZ_BINARY_PATH=$PWD/target/release/rootsqz`.

The crate and all npm packages have the same version. Set it with `node scripts/set-version.mjs <version>`.

### Releasing
In [CHANGELOG.md](CHANGELOG.md), change `## [Unreleased]` to the version and the date, add an empty `## [Unreleased]` above it and a link reference for the version at the end. Then:
```sh
node scripts/set-version.mjs 0.5.0
git commit -am "Release 0.5.0"
git tag v0.5.0
git push origin main v0.5.0
```
The tag starts the release workflow. It builds the executables for all targets, publishes the npm packages and creates the GitHub release, with the section of the version in CHANGELOG.md as its notes. A version without a section fails the workflow before anything is published. The tag must be `v` followed by the full version, otherwise nothing is published to npm. A version with a prerelease part, e.g. `0.5.0-rc.1`, is published under the npm tag `next`.

Publishing uses npm [trusted publishing](https://docs.npmjs.com/trusted-publishers): each package names this repository and `release.yml` as its trusted publisher on npmjs.com, so no npm token is stored. A new package cannot have a trusted publisher before it exists, so its first version needs a token (as the repository secret `NPM_TOKEN`, passed to the publish step as `NODE_AUTH_TOKEN`) or a manual `npm publish`. Afterwards, configure its trusted publisher with `npm trust github @r00tkids/<name> --file release.yml --repo r00tkids/rootsqz --allow-publish` and remove the token again.

## TODO
- [ ] Support larger hashmaps (>256 MiB)
- [ ] Add support for custom loading bar JS hook

## References
- [ZPAQ Compression Algorithm](https://mattmahoney.net/dc/zpaq_compression.pdf)
- [Crinkler](https://github.com/runestubbe/Crinkler)
- [About arithmetic coders and recip_arith in particular](https://cbloomrants.blogspot.com/2018/10/about-arithmetic-coders-and-reciparith.html)
- [Rant on New Arithmetic Coders](https://cbloomrants.blogspot.com/2008/10/10-05-08-5.html)

---
For questions, issues, or contributions, please open an issue or pull request on GitHub.
