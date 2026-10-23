// Instantiates the WebAssembly decoder of wasm_decoder.rs, whose bytes are the characters of the
// string that output_generator.rs puts in, puts the compressed stream a into its memory and calls f with its exports:
// the memory m and d(n), which decodes up to n more bits and returns the number of decoded bytes.
let wasmDecoder = (a, f) => WebAssembly.instantiate(Uint8Array.from("@@WASM@@", c => c.charCodeAt())).then(({ instance: { exports: e } }) => (
    new Uint8Array(e.m.buffer).set(a.subarray(0, WASM_ENCODED_LEN), WASM_INPUT), f(e)));

// The decoded bytes, out of the memory of the decoder
let wasmOutput = e => new Uint8Array(e.m.buffer, WASM_OUTPUT, WASM_DECODED_LEN).slice();
