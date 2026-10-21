// Predicts the bit of the byte that followed the last occurrence of the previous `contextBytes`
// bytes, within the last 64 KiB
let MatchPredictor = (contextBytes, confidence, tableBits) => {
    let history = new Uint8Array(65536), keys = new Uint32Array(1 << tableBits), positions = new Int32Array(1 << tableBits),
        contextMask = 2 ** (8 * contextBytes) - 1, position = 0, context = 0, match = -1, partial = 1,
        expected = () => history[match & 65535] >> Math.clz32(partial) - 24 & 1;
    return {
        p: () => match < 0 ? 0 : probStretch(expected() ? confidence : 1 - confidence),
        l: bit => {
            match >= 0 && expected() != bit && (match = -1);
            if ((partial = partial << 1 | bit) > 255) {
                history[position++ & 65535] = partial & 255;
                context = (context << 8 | partial & 255) & contextMask;
                let slot = Math.imul(context, 0x9E35A7BD) & keys.length - 1, prior = positions[slot];
                match >= 0 ? match++ : prior && keys[slot] == context >>> 0 && position - prior < 65536 && (match = prior - 1);
                keys[slot] = context;
                positions[slot] = position + 1;
                partial = 1;
            }
        }
    };
};
