// Decodes like coder_worker.js, in a web worker, with the WebAssembly decoder m.
// Posts the number of decoded bytes about every 16 ms, and then the decoded bytes.
let decompressWorker = (m, a) => wasmDecoder(m, a, e => {
    for (let next = 0, i; (i = e.d(4096)) < WASM_DECODED_LEN;)
        if (performance.now() > next) next = performance.now() + 16, postMessage(i);
    postMessage(e = wasmOutput(e), [e.buffer]);
});
