# websqz
websqz is a tool for compressing and decompressing demo intros for the web. The current overhead is about 1.6 KiB, so it's primarily intended for 64KiB intros, though this may change in the future. 
It's inspired by [Crinkler](https://github.com/runestubbe/Crinkler) and ZPaq series of compressors.

Also see the rollup / Vite plugin: https://github.com/r00tkids/rollup-plugin-websqz

## Features
- High compression ratio for JavaScript and binary assets - about 20% better than deflate-raw
- Extensible for new compression strategies

## Installation

1. Clone the repository:
   ```sh
   git clone https://github.com/r00tkids/websqz.git
   cd websqz
   ```
2. Install dependencies:
   - Install [UglifyJS](https://github.com/mishoo/UglifyJS):
     ```sh
     npm install -g uglify-js
     ```
   - Build and install websqz (requires Rust and Cargo):
     ```sh
     cargo install --path .
     ```

## Usage

Basic compression example:
```sh
websqz --js-main example/index.js -f example/bundled.glsl --output-directory out
```

Pass `--brotli` to append a Brotli stream to the single `index.html` for web
inputs of any size.
The browser decodes it with `DecompressionStream('brotli')`. This works in
Firefox 147+ and Safari 18.4+. As of September 2026, Chrome does not support
Brotli in `DecompressionStream`, so the arithmetic compressor remains the
default for browser compatibility.

Options:
- `--js-main <file>`: Entry point JavaScript file
- `--files <FILES>`: Extra files to be compressed. Order matters, so files of similar content should be ordered together.
- `--pre-compressed-files <FILES>`: Extra files that are already compressed (jpeg, mp4 etc.)
- `--output-directory <dir>`: Output directory for compressed files
- `--size-profile <4k|64k>`: Embedded compression preset (default: `4k`). The `64k` preset adds a bounded match predictor for larger JavaScript inputs.
- `--brotli`: Use native Brotli decoding for web inputs (Firefox 147+ / Safari 18.4+).
- `--config <file.json>`: Use a complete JSON `CompressConfig` instead of the embedded preset. This takes precedence over `--size-profile`.
- See `websqz --help` for more CLI options

Both presets are available for `web` and `node` targets. To customize a model, copy [the 4k preset](src/compressor/default_config.json) or [the 64k preset](src/compressor/size_64k_config.json), edit its `model.models` array, and pass it with `--config`. Supported optional model entries include `{"type":"CharClass","order":3}`, `{"type":"TokenPosition"}`, `{"type":"MatchPredictor","context_bytes":2,"confidence":0.97,"table_bits":16}`, and `{"type":"IndirectByte","context_bytes":2,"max_count":15}`. The decoder is generated from the selected config.

## Runtime API
To access the contents of files specified with `--files` or `--pre-compressed-files`, use `wsqz.files["<FILENAME>"]`.
`wsqz.files["<FILENAME>"]` returns an `Uint8Array`, to read it as text use `new TextDecoder().decode(wsqz.files["example.glsl"])`.

Note: `<FILENAME>` refers to the base name of the file, not its full or relative path.

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
