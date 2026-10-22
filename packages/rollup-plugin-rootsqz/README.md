# @r00tkids/rollup-plugin-rootsqz
![NPM Version](https://img.shields.io/npm/v/%40r00tkids%2Frollup-plugin-rootsqz)

Rollup / Vite plugin for using [rootsqz](https://github.com/r00tkids/rootsqz) to compress and bundle code and assets into one HTML file. This is intented for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

## Install
`npm i -D @r00tkids/rollup-plugin-rootsqz`

This installs the rootsqz executable for your platform through [`@r00tkids/rootsqz`](https://www.npmjs.com/package/@r00tkids/rootsqz). The plugin and the executable share one version.

## Usage
```js
// vite.config.js
import { defineConfig } from 'vite';
import rootsqz from '@r00tkids/rollup-plugin-rootsqz';

export default defineConfig({
  build: {
    // Disable module preloading for better compression results
    modulePreload: false,
  },
  plugins: [rootsqz()]
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
`&raw` keeps Vite's own JSON handling away from the import, without it the build fails. The same applies to CSS (`style.css?rootsqz-txt&raw`). See [json-as-string-example.js](https://github.com/r00tkids/rootsqz/blob/main/examples/vite/src/json-as-string-example.js).

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
- To show a loader while the output decompresses, set `loader: { preset: "spinner" }`, `{ preset: "bar" }` or `{ preset: "progress" }`, or your own `{ html, js }` files. See [Loader](https://github.com/r00tkids/rootsqz#loader). To keep the page responsive while it decodes, set `decoder: "worker"`, see [Decoder](https://github.com/r00tkids/rootsqz#decoder).
- `title: "My intro"` sets the page title before the output decompresses, and `bootJs: "boot.js"` runs a script first, see [Title and boot script](https://github.com/r00tkids/rootsqz#title-and-boot-script).
- `search: true` searches for the model config that packs the bundle smallest before packing, see [Config search](https://github.com/r00tkids/rootsqz#config-search). It takes about an hour for a 64k intro, so set `searchCache` to a directory that keeps the config found for the next build of the same bundle. It also keeps the progress there, so a stopped build continues the search; Vite empties `dist`, so without `searchCache` a stopped search starts over.
- With `report: true`, or with the environment variable `ROOTSQZ_REPORT=1` (e.g. `ROOTSQZ_REPORT=1 vite build`), the plugin writes a [compression report](https://github.com/r00tkids/rootsqz#compression-report) to `rootsqz-output/report`. It names the imported files by their module and maps the bundle back to its original sources, for which it turns on `build.sourcemap: 'hidden'` if no source map is set. For an imported `x.wasm`, it gives the report `x.debug.wasm` (the unstripped file) or `x.wasm.map` next to it as debug info, or the file that `reportWasmDebug: { 'src/x.wasm': 'build/x.full.wasm' }` names, so the report names its functions and maps them to source lines. Run `npx rootsqz install-skill` once to teach coding agents to use it ([For coding agents](https://github.com/r00tkids/rootsqz#for-coding-agents)).

See the [example](https://github.com/r00tkids/rootsqz/tree/main/examples/vite) for a working example with support for `vite-plugin-glsl`.

## Example Options
```js
rootsqz({
    /**
     * Full path to the rootsqz executable.
     * By default the plugin uses the executable installed with the npm package,
     * or the one named by the environment variable ROOTSQZ_BINARY_PATH.
     */
    rootsqzPath: undefined,

    /**
     * Embedded compression preset: "64k" (default), "4k" for small intros, "64k-wasm",
     * which also models WebAssembly files, "64k-semi-fast" or "64k-fast", which decode about
     * 1.7 and 2 times as fast as "64k" but pack about 2% and 6% larger. The decoder of "64k"
     * and "64k-wasm" needs up to about 1 GB of memory, that of the fast ones 0.6-0.7 GB.
     */
    sizeProfile: "64k",

    /**
     * Pack the output as a Brotli stream that the browser decodes with
     * DecompressionStream('brotli') (Firefox 147+ / Safari 18.4+, not Chrome).
     * Cannot be combined with `config`, `report` or a 64k size profile.
     */
    brotli: false,

    /**
     * Path to a complete JSON compression config.
     * Takes precedence over `sizeProfile`.
     */
    config: undefined,

    /**
     * Write a compression report to rootsqz-output/report/: report.json, a summary
     * for analysis (also by an LLM), report.html, and data.bin for `rootsqz report`.
     * The environment variable ROOTSQZ_REPORT=1 also turns it on.
     */
    report: false,

    /**
     * Also record what every submodel predicted. Implies `report`.
     */
    reportModels: false,

    /**
     * Also write these views of the report as TSV files, e.g. ["lines", "tokens"].
     * Implies `report`.
     */
    reportDetail: [],

    /**
     * Debug info of imported wasm files, by their path: the unstripped .wasm or a .wasm.map.
     * Without it, x.debug.wasm and x.wasm.map next to x.wasm are used. Implies `report`.
     */
    reportWasmDebug: {},

    /**
     * Loader shown while the output decompresses: a built-in `preset` ("spinner", "bar" or "progress"),
     * or your own `html` and `js` files (paths relative to the working directory).
     * `keep` leaves it in place when the bundle runs, so the demo can continue the progress.
     * See https://github.com/r00tkids/rootsqz#loader
     */
    loader: { preset: "bar", keep: false },

    /**
     * How the output decodes: "serial" (in one go), "sliced" (in slices on the main thread)
     * or "worker" (in a web worker). By default "sliced" with a loader script, otherwise "serial".
     * See https://github.com/r00tkids/rootsqz#decoder
     */
    decoder: "worker",

    /**
     * Title of the page, set before the output decompresses.
     */
    title: "My intro",

    /**
     * File with JavaScript that runs first, before the loader and decompression, e.g. to set
     * up the page. It is not bundled, but minified with the boot code.
     */
    bootJs: "boot.js",

    /**
     * Search for the model config that packs the bundle smallest, counting the decoder bytes of
     * each change. `searchCache` keeps the config found for the next build of the same bundle,
     * `searchThreads` sets how many configs are tried at once.
     * See https://github.com/r00tkids/rootsqz#config-search
     */
    search: false,
    searchThreads: undefined,
    searchCache: undefined,

    /**
     * Name of the global that the boot code of the executable puts the files in.
     * By default `rsqz`. Set it to `wsqz` when `rootsqzPath` names an executable up to 0.4.0.
     */
    runtimeGlobal: undefined,

    /**
     * Further arguments passed to the executable as they are.
     */
    rootsqzArgs: [],

    /**
     * Hooks that process files before they are imported in code or compressed by rootsqz.
     * The first hook that matches a module id and returns a result is used.
     */
    fileTransforms: [
        {
            /**
             * Module ids that the transform applies to. By default all.
             */
            include: /\.glsl$/,

            /**
             * Module ids that the transform does not apply to.
             */
            exclude: undefined,

            /**
             * Returns the content to pack for the module `id`, given the content of its file as a Buffer.
             * `ctx` is the Rollup plugin context.
             */
            transform: async (ctx, id, content) => {
                return {
                    /**
                     * The content to pack, as a Buffer.
                     */
                    content: Buffer.from("Hello World", "utf-8"),

                    /**
                     * Whether the content is already compressed and should be stored as it is.
                     */
                    isCompressed: false,

                    /**
                     * Whether the content is text. The import then gives a string instead of an Uint8Array.
                     */
                    isText: true,

                    /**
                     * Optional, by default taken from the file path.
                     */
                    fileExt: ".glsl"
                };
            }
        }
    ]
})
```
