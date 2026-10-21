// The previous 8 bytes, of which bit i of byteMask keeps byte i (0 is the last one)
let NOrderByte = (byteMask, maxCount = 15, hi = 0, lo = 0, maskHi = 0, maskLo = 0, magic = hash(byteMask, 2)) => {
    for (let i = 0; i < 4; i++)
        maskLo |= (byteMask >> i & 1) * 255 << 8 * i, maskHi |= (byteMask >> i + 4 & 1) * 255 << 8 * i;
    return CounterModel(maxCount, byte =>
        contextHash((hi = hi << 8 | lo >>> 24) & maskHi, (lo = lo << 8 | byte) & maskLo, magic));
};
