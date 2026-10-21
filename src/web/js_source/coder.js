// Returns the decoder of the bits of `data`, which takes the probability that the next bit is 1
let ArithmeticDecoder = (data, len, state = 0, low = 0, high = 2 ** 32 - 1, ptr = 0) => {
    let next = () => state = (state << 8 | data[ptr++]) >>> 0;
    // Bytes past the stream read as 0
    data = data.subarray(0, len);
    next(), next(), next(), next();
    return p => {
        let mid = Math.min(low + (high - low) * p >>> 0, high - 1), bit = state <= mid;
        bit ? high = mid : low = mid + 1;
        for (; (high ^ low) >>> 0 < 1 << 24; next())
            low = low << 8 >>> 0, high = (high << 8 | 255) >>> 0;
        return +bit;
    };
};
