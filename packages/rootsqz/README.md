# @rootkids/rootsqz
![NPM Version](https://img.shields.io/npm/v/%40rootkids%2Frootsqz)

[rootsqz](https://github.com/r00tkids/rootsqz) compresses and packs JavaScript and assets into one HTML file. This is intended for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

This package installs the `rootsqz` executable for your platform and a small Node API to run it. For Rollup and Vite, see [`@rootkids/rollup-plugin-rootsqz`](https://www.npmjs.com/package/@rootkids/rollup-plugin-rootsqz).

## Install
`npm i -D @rootkids/rootsqz`

Prebuilt executables exist for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64). They are optional dependencies, so do not install with `--omit=optional`.

## Command line
```sh
npx rootsqz --js-main index.js --files shader.glsl --output-directory out
```

See `npx rootsqz --help` and the [rootsqz README](https://github.com/r00tkids/rootsqz#usage) for the options.

## Node API
```js
import { compress } from '@rootkids/rootsqz';

await compress({
    jsMain: 'index.js',
    outputDirectory: 'out',
    // Packed with compression. Files of similar content should follow each other.
    files: ['shader.glsl'],
    // Packed without compression
    preCompressedFiles: ['image.jpg'],
    // "4k" (default) or "64k"
    sizeProfile: '64k',
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

In the packed JavaScript, `rsqz.files["<file name>"]` is the content of a packed file as an `Uint8Array`.

## Environment variables
- `ROOTSQZ_BINARY_PATH`: path of a rootsqz executable to use instead of the installed one.
- `ROOTSQZ_UGLIFYJS`: UglifyJS program that the executable runs instead of the one bundled with this package.
