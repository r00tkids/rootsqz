let HashMap = (pow2Size, itemSize, defaultValue, itemEncoder, itemDecoder) => {
    let data = new Uint8Array(2 ** pow2Size * itemSize);
    let mask = (1 << pow2Size) - 1;

    let defaultValueBuffer = new Uint8Array(itemSize);
    itemEncoder(new DataView(defaultValueBuffer.buffer), defaultValue);
    for (let i = 0; i < 2 ** pow2Size; i++) {
        data.set(defaultValueBuffer, i * itemSize);
    }

    return {
        get: (key) => {
            return itemDecoder(new DataView(data.buffer, (key & mask) * itemSize));
        },
        set: (key, value) => {
            itemEncoder(new DataView(data.buffer, (key & mask) * itemSize), value);
        }
    };
}

function hash(value, shift) {
    const K_MUL = 0x9E35A7BDn;
    value ^= value >> BigInt(shift);
    return ((K_MUL * value) & 0xffffffffn) >> BigInt(shift);
}