// Decodes in one go, as decompress.js, with the WebAssembly decoder. Returns a promise of the decoded bytes.
let decompress = a => wasmDecoder(WASM_MODULE, a, e => (e.d(-1), wasmOutput(e)));
