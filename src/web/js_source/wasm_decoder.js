// Instantiates the WebAssembly decoder m of wasm_decoder.rs, puts the compressed stream a into its
// memory and calls f with its exports: the memory m and d(n), which decodes up to n more bits and
// returns the number of decoded bytes.
let wasmDecoder = (m, a, f) => WebAssembly.instantiate(m).then(({ instance: { exports: e } }) => (
    new Uint8Array(e.m.buffer).set(a.subarray(0, WASM_ENCODED_LEN), WASM_INPUT), f(e)));

// The decoded bytes, out of the memory of the decoder
let wasmOutput = e => new Uint8Array(e.m.buffer, WASM_OUTPUT, WASM_DECODED_LEN).slice();
