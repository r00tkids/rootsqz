// Decodes like decompress (decompress.js), but in slices of about 12 ms, so the browser can paint in between.
// Calls rsqz.progress(done, total), if set, after each slice, and then done with the decoded bytes.
let decompressSliced = (model, data, encodedLen, decodedLen, done) => {
    let decode = ArithmeticDecoder(data, encodedLen), output = new Uint8Array(decodedLen), i = 0, bit;
    // A message to a MessageChannel yields to the browser without the 4 ms clamp of nested setTimeout
    let channel = new MessageChannel();

    channel.port1.onmessage = () => {
        let sliceEnd = performance.now() + 12;
        while (i < decodedLen * 8) {
            model.l(bit = decode(probSquash(model.p()))), output[i >> 3] = output[i >> 3] << 1 | bit;
            if (!(++i & 2047) && performance.now() > sliceEnd) break;
        }
        rsqz.progress?.(i >> 3, decodedLen);
        i < decodedLen * 8 ? channel.port2.postMessage(0) : done(output);
    };
    channel.port2.postMessage(0);
};
