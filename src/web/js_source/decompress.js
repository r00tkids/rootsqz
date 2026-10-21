let decompress = (model, data, encodedLen, decodedLen) => {
    let decode = ArithmeticDecoder(data, encodedLen), output = new Uint8Array(decodedLen), bit;
    for (let i = 0; i < decodedLen * 8; i++)
        model.l(bit = decode(probSquash(model.p()))), output[i >> 3] = output[i >> 3] << 1 | bit;
    return output;
};
