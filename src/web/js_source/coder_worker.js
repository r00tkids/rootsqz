// Decodes like decompress (decompress.js), in a web worker.
// Posts the number of decoded bytes about every 16 ms, and then the decoded bytes.
let decompressWorker = (model, data, encodedLen, decodedLen) => {
    let decoder = ArithmeticDecoder(data, encodedLen);
    let output = new Uint8Array(decodedLen);
    let next = 0;

    for (let byteIdx = 0;byteIdx < decodedLen;) {
        let byte = 0;
        for (let i = 0;i < 8;++i) {
            let bit = decoder.decode(probSquash(model.pred()));
            model.learn(bit);
            byte = (byte << 1) | bit;
        }
        output[byteIdx++] = byte;
        if (!(byteIdx & 255) && performance.now() > next) {
            next = performance.now() + 16;
            postMessage(byteIdx);
        }
    }
    postMessage(output, [output.buffer]);
};
