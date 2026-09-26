let MatchPredictor = (contextBytes, confidence, tableBits) => {
    let history = new Uint8Array(65536);
    let keys = new Uint32Array(1 << tableBits);
    let positions = new Float64Array(1 << tableBits);
    let tableMask = (1 << tableBits) - 1;
    let contextMask = contextBytes === 4 ? 0xffffffff : (2 ** (8 * contextBytes) - 1);
    let position = 0, context = 0, matchPosition = -1, partialByte = 1;
    return {
        pred: () => {
            if (matchPosition < 0 || matchPosition >= position || position - matchPosition > history.length) return 0;
            let shift = 7 - Math.floor(Math.log2(partialByte));
            let bit = (history[matchPosition & 65535] >> shift) & 1;
            return probStretch(bit ? confidence : 1 - confidence);
        },
        learn: bit => {
            if (matchPosition >= 0) {
                let shift = 7 - Math.floor(Math.log2(partialByte));
                if (matchPosition >= position || position - matchPosition > history.length || ((history[matchPosition & 65535] >> shift) & 1) !== bit) {
                    matchPosition = -1;
                }
            }
            partialByte = (partialByte << 1) | bit;
            if (partialByte >= 256) {
                let byte = partialByte & 255;
                history[position & 65535] = byte;
                position++;
                context = ((context << 8) | byte) & contextMask;
                let slot = Math.imul(context, 0x9E35A7BD) & tableMask;
                if (matchPosition >= 0) matchPosition++;
                else {
                    let prior = positions[slot];
                    if (prior && keys[slot] === (context >>> 0) && position - (prior - 1) <= history.length)
                        matchPosition = prior - 1;
                }
                keys[slot] = context;
                positions[slot] = position + 1;
                partialByte = 1;
            }
        }
    };
};
