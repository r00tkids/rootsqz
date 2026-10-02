# rollup-plugin-rootsqz
![NPM Version](https://img.shields.io/npm/v/rollup-plugin-rootsqz)

Rollup / Vite plugin for using [rootsqz](https://github.com/r00tkids/rootsqz) to compress and bundle code and assets into one HTML file. This is intented for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

## Install
`npm i rollup-plugin-rootsqz`

## Usage
```js
// vite.config.js
import { defineConfig } from 'vite';
import rootsqz from 'rollup-plugin-rootsqz';

export default defineConfig({
  plugins: [rootsqz()]
});
```

See the [example](https://github.com/r00tkids/rootsqz/tree/main/rollup-plugin-rootsqz/example) for a working example with support for `vite-plugin-glsl`.

## Example Options
```js
rootsqz({
    /*
    Full path to the rootsqz executable (currently named `websqz`).
    If null (default), the plugin uses the executable
    installed when installing the npm package.
    Otherwise it will try to resolve `websqz` from your system PATH.
    */
    rootsqzPath: null,
    /*
    Embedded compression preset, "4k" (default) or "64k".
    The 64k preset adds a bounded match predictor for larger JavaScript inputs.
    */
    sizeProfile: "64k",
    /*
    Pack the output as a Brotli stream that the browser decodes with
    DecompressionStream('brotli') (Firefox 147+ / Safari 18.4+, not Chrome).
    Cannot be combined with `config`, `report` or the 64k size profile.
    */
    brotli: false,
    /*
    Path to a complete JSON compression config.
    Takes precedence over `sizeProfile`.
    */
    config: undefined,
    /*
    Write a detailed compression report next to the output.
    */
    report: false,
    /*
    Name of the global that the boot code of the executable puts the files in.
    By default the plugin asks the executable for its version:
    `wsqz` up to 0.4.0, `rsqz` from 0.4.1.
    */
    runtimeGlobal: undefined,
    /*
    Further arguments passed to the executable as they are.
    */
    rootsqzArgs: [],
    fileTransforms: [
        {
            include: /\.glsl$/,
            transform: async (ctx, id, content) => {
                return {
                    content: Buffer.from("Hello World", "utf-8"),
                    isCompressed: false,
                    isText: true,
                    fileExt: ".glsl" // Optional
                };
            }
        }
    ]
})
```

`sizeProfile`, `brotli` and `config` need an executable newer than the 0.4 release that
the install script downloads. Build one from the [rootsqz](https://github.com/r00tkids/rootsqz)
repository with `cargo build --release` and point `rootsqzPath` at `target/release/rootsqz`.
