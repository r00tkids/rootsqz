# @r00tkids/rootsqz
![NPM Version](https://img.shields.io/npm/v/%40r00tkids%2Frootsqz)

[rootsqz](https://github.com/r00tkids/rootsqz) compresses and packs JavaScript and assets into one HTML file. This is intended for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

This package installs the `rootsqz` executable for your platform and a small Node API to run it. For Rollup and Vite, see [`@r00tkids/rollup-plugin-rootsqz`](https://www.npmjs.com/package/@r00tkids/rollup-plugin-rootsqz).

## Install
`npm i -D @r00tkids/rootsqz`

Prebuilt executables exist for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64). They are optional dependencies, so do not install with `--omit=optional`.

## Command line
```sh
npx rootsqz --js-main index.js --file shader.glsl --output-directory out
```

See `npx rootsqz --help` and the [rootsqz README](https://github.com/r00tkids/rootsqz#usage) for the options.

With `--report`, rootsqz writes a [compression report](https://github.com/r00tkids/rootsqz#compression-report) to `out/report`, and `npx rootsqz report out --view hotspots --top 20` prints views of it. `npx rootsqz install-skill` installs an agent skill into your project, so that coding agents such as Claude Code can make and read the report themselves.

## Node API
```js
import { compress } from '@r00tkids/rootsqz';

await compress({
    /**
     * JavaScript file that is evaluated after decompression.
     */
    jsMain: 'index.js',

    /**
     * Directory that the output is written to.
     */
    outputDirectory: 'out',

    /**
     * Packed with compression. Files of similar content should follow each other.
     */
    files: ['shader.glsl'],

    /**
     * Packed without compression
     */
    preCompressedFiles: ['image.jpg'],

    /**
     * "64k" (default), "4k" for small intros, or "64k-wasm" for 64k intros with WebAssembly files
     */
    sizeProfile: '64k',

    /**
     * Write a compression report to out/report. reportModels and reportDetail add detail,
     * reportSourceMap, reportLabels and reportWasmDebug ({ 'x.wasm': 'x.debug.wasm' }) add names.
     */
    report: true,

    /**
     * Loader shown while the web output decompresses: `preset` "spinner", "bar" or "progress",
     * or your own `html` and `js` files. `keep` leaves it in place when the main JavaScript runs.
     */
    loader: { preset: 'bar' },

    /**
     * How the web output decodes: "serial", "sliced" or "worker" (in a web worker).
     * By default "sliced" with a loader script, otherwise "serial".
     */
    decoder: 'worker',

    /**
     * Title of the web page, and a script that runs first, before the loader and decompression.
     */
    title: 'My intro',
    bootJs: 'boot.js',
});
```

| Export | Description |
|---|---|
| `compress(options)` | Runs the executable with the given options. Rejects if it fails. |
| `compressArgs(options)` | The command line arguments that `compress` passes. |
| `run(args, options)` | Runs the executable with command line arguments. |
| `binaryPath()` | Path of the executable for this platform. |
| `toolEnvironment(env)` | Environment that points the executable to the bundled uglify-js. |
| `RUNTIME_GLOBAL` | Name of the global that holds the packed files in the output: `rsqz`. |

In the packed JavaScript, `rsqz.files["<file name>"]` is the content of a packed file as an `Uint8Array`. A loader script sets `rsqz.progress = (done, total) => ...`, see [Loader](https://github.com/r00tkids/rootsqz#loader).

## Environment variables
- `ROOTSQZ_BINARY_PATH`: path of a rootsqz executable to use instead of the installed one.
- `ROOTSQZ_UGLIFYJS`: UglifyJS program that the executable runs instead of the one bundled with this package.
