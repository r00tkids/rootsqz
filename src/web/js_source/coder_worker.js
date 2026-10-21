// Decodes like decompress (decompress.js), in a web worker.
// Posts the number of decoded bytes about every 16 ms, and then the decoded bytes.
let decompressWorker = (model, data, encodedLen, decodedLen) => {
    let decode = ArithmeticDecoder(data, encodedLen), output = new Uint8Array(decodedLen), next = 0, bit;
    for (let i = 0; i < decodedLen * 8;) {
        model.l(bit = decode(probSquash(model.p()))), output[i >> 3] = output[i >> 3] << 1 | bit;
        if (!(++i & 2047) && performance.now() > next) {
            next = performance.now() + 16;
            postMessage(i >> 3);
        }
    }
    postMessage(output, [output.buffer]);
};
