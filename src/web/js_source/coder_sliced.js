
// Decodes like decompress, but in slices of about 12 ms, so the browser can paint in between.
// Calls rsqz.progress(done, total), if set, after each slice. Resolves to the decoded bytes.
let decompressSliced = (model, data, encodedLen, decodedLen) => new Promise(resolve => {
    let decoder = ArithmeticDecoder(data, encodedLen);
    let output = new Uint8Array(decodedLen);
    let byteIdx = 0;
    // A message to a MessageChannel yields to the browser without the 4 ms clamp of nested setTimeout
    let channel = new MessageChannel();

    channel.port1.onmessage = () => {
        let sliceEnd = performance.now() + 12;
        while (byteIdx < decodedLen) {
            let byte = 0;
            for (let i = 0;i < 8;++i) {
                let bit = decoder.decode(probSquash(model.pred()));
                model.learn(bit);
                byte = (byte << 1) | bit;
            }
            output[byteIdx++] = byte;
            if (!(byteIdx & 255) && performance.now() > sliceEnd) break;
        }
        rsqz.progress?.(byteIdx, decodedLen);
        if (byteIdx < decodedLen) channel.port2.postMessage(0);
        else resolve(output);
    };
    channel.port2.postMessage(0);
});
