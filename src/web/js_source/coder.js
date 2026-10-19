let ArithmeticDecoder = (input, len) => {
    let state = 0;
    let low = 0;
    let high = 0xFFFFFFFF;
    let readPtr = 0;

    for (let i = 0;i < 4;++i) {
        let c = readPtr >= input.byteLength ? 0 : input[readPtr++];
        state = ((state << 8) | c) >>> 0;
    }

    return {
        decode: (p) => {
            if (p < 0. && p >= 1.) throw new Error("probability out of range");
            if (high <= low) throw new Error("high <= low");

            let mid = (low + (high - low) * p) >>> 0;
            if (mid >= high) {
                // We loose some precision to prevent overflow
                // Unlikely to happen in practice
                mid = high - 1;
            }

            if (!(high > mid && mid >= low)) throw new Error("mid out of range");

            let bit = 0;
            if (state <= mid) {
                bit = 1;
                high = mid;
            } else {
                low = (mid + 1) >>> 0;
            }

            while (((high ^ low) >>> 0) < (1 << 24)) {
                low = (low << 8) >>> 0;
                high = ((high << 8) | 0xFF) >>> 0;
                let c = readPtr >= len ? 0 : input[readPtr++];
                state = ((state << 8) | c) >>> 0;
            }

            return bit;
        }
    };
};
