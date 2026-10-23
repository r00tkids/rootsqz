// Decodes like coder_sliced.js, in slices of about 12 ms, with the WebAssembly decoder
let decompressSliced = (a, done) => wasmDecoder(a, e => {
    let channel = new MessageChannel(), i;
    channel.port1.onmessage = () => {
        let sliceEnd = performance.now() + 12;
        while ((i = e.d(4096)) < WASM_DECODED_LEN && performance.now() < sliceEnd);
        rsqz.progress?.(i, WASM_DECODED_LEN);
        i < WASM_DECODED_LEN ? channel.port2.postMessage(0) : done(wasmOutput(e));
    };
    channel.port2.postMessage(0);
});
