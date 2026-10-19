let HashMap = (pow2Size, itemSize, defaultValue, itemEncoder, itemDecoder) => {
    let data = new Uint8Array(2 ** pow2Size * itemSize);
    let mask = (1 << pow2Size) - 1;

    // Write the default value into the first item, then double the filled part until the table is full
    itemEncoder(new DataView(data.buffer), defaultValue);
    for (let n = itemSize; n < data.length; n *= 2) {
        data.copyWithin(n, 0, n);
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