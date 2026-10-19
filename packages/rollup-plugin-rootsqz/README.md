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
- To show a loader while the output decompresses, set `loader: { preset: "spinner" }`, `{ preset: "bar" }` or `{ preset: "progress" }`, or your own `{ html, js }` files. See [Loader](https://github.com/r00tkids/rootsqz#loader).

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
     * Embedded compression preset, "4k" (default) or "64k".
     * The 64k preset adds a bounded match predictor for larger JavaScript inputs.
     */
    sizeProfile: "64k",

    /**
     * Pack the output as a Brotli stream that the browser decodes with
     * DecompressionStream('brotli') (Firefox 147+ / Safari 18.4+, not Chrome).
     * Cannot be combined with `config`, `report` or the 64k size profile.
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
     * Loader shown while the output decompresses: a built-in `preset` ("spinner", "bar" or "progress"),
     * or your own `html` and `js` files (paths relative to the working directory).
     * `keep` leaves it in place when the bundle runs, so the demo can continue the progress.
     * See https://github.com/r00tkids/rootsqz#loader
     */
    loader: { preset: "bar", keep: false },

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
