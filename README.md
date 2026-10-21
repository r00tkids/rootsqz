# rootsqz
rootsqz packs a JavaScript intro and its assets into one small, self-extracting HTML file. It's made for 64k intros, and has a preset for 4k intros too.

It works like the compressors of [Crinkler](https://github.com/runestubbe/Crinkler), ZPAQ and paq8: a set of models predicts each bit from what came before, and the better they predict, the smaller the file.

There is also a [Vite / Rollup plugin](#vite--rollup-plugin).

## Features
- 35–45% smaller than deflate on 64k intros, decoder included
- Models made for JavaScript, WebAssembly and tables of numbers
- A [search](#config-search) for the settings that pack your intro smallest
- A [report](#compression-report) of where the bytes go, which coding agents can read too
- An optional [loader](#loader), and decoding in a [web worker](#decoder)

The decoder itself takes about 1.4 KB with the `4k` preset and 3–4 KB with `64k`. So a very small input comes out larger than it went in.

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
```sh
rootsqz --js-main index.js -f shader.glsl -o out
```
This packs `index.js` and `shader.glsl` into `out/index.html`.

The main options:
- `-j, --js-main <FILE>`: your main JavaScript file.
- `-f, --file <FILE>`: a file to pack with it. Repeat it for more files, and put similar files next to each other.
- `-p, --pre-compressed-file <FILE>`: a file that is already compressed, like a JPEG, stored as it is.
- `-o, --output-directory <DIR>`: where `index.html` goes.
- `--size-profile <64k|4k|64k-wasm>`: the preset. `64k` is the default; its decoder needs up to about 1 GB of memory. `4k` has a smaller decoder for small intros. `64k-wasm` is `64k` plus models for WebAssembly; use it when your intro has a large wasm file.
- `--search`: find the settings that pack your intro smallest. See [Config search](#config-search).
- `--loader` and `--decoder`: show something while the intro unpacks. See [Loader](#loader) and [Decoder](#decoder).
- `-r, --report`: write a report of where the bytes go. See [Compression report](#compression-report).
- `--brotli`: use Brotli, which the browser unpacks itself. Smaller decoder, worse compression, and only Firefox 147+ and Safari 18.4+ support it (not Chrome, as of September 2026).

`rootsqz --help` lists all options.

Your files are available at runtime as `rsqz.files["name"]`, see [Runtime API](#runtime-api).

### Your own settings
The presets are JSON files: [4k](src/compressor/default_config.json), [64k](src/compressor/size_64k_config.json) and [64k-wasm](src/compressor/size_64k_wasm_config.json). To change one, copy it, edit it, and pass it with `--config`. The decoder is generated from the config, so it only contains the models you use. [docs/config.md](docs/config.md) describes the models and settings, but usually it's easier to let `--search` do this.

### Config search
`--search` tries small changes to the settings, one at a time, and keeps each one that makes the output smaller, decoder included. Models that cost more decoder code than they save are dropped. It takes about an hour for a 64k intro. The result is saved as `search_config.json`; pass it with `--config` to build again without searching, or set `--search-cache <DIR>` to reuse it automatically.

To search without building, or to find one config for several intros, use `rootsqz search`. It saves the best config so far after each improvement, so you can stop it at any time and build with what it found; `--search` during a build only saves its result at the end. See [docs/config.md](docs/config.md#search).

## Compression report
`--report` writes a report to `report/` in the output directory. Open `report.html` to see your input colored by how many bits each byte costs, with suggestions for what to change. `rootsqz report <dir>` prints the details, such as the most expensive parts or the cost of each wasm function. See [docs/report.md](docs/report.md).

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
The web output can show a loader while it decompresses. Without loader options, it has no loader code.

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

`--decoder` is only for the `web` target without `--brotli`.

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

## References
- [ZPAQ Compression Algorithm](https://mattmahoney.net/dc/zpaq_compression.pdf)
- [PAQ8](https://mattmahoney.net/dc/paq.html), whose ContextMixer and APMs the 64k presets follow
- [Crinkler](https://github.com/runestubbe/Crinkler)
- [About arithmetic coders and recip_arith in particular](https://cbloomrants.blogspot.com/2018/10/about-arithmetic-coders-and-reciparith.html)
- [Rant on New Arithmetic Coders](https://cbloomrants.blogspot.com/2008/10/10-05-08-5.html)

---
For questions, issues, or contributions, please open an issue or pull request on GitHub.
