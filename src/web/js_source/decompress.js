
let decompress = (model, data, encodedLen, decodedLen) => {
    let decoder = ArithmeticDecoder(data, encodedLen);
    let output = [];

    for (let byteIdx = 0;byteIdx < decodedLen;++byteIdx) {
        let byte = 0;
        for (let i = 0;i < 8;++i) {
            let prob = probSquash(model.pred());
            let bit = decoder.decode(prob);
            model.learn(bit);
            byte = (byte << 1) | bit;
        }
        output.push(byte);
    }

    return new Uint8Array(output);
};