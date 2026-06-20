let ASCII_CASE_MASK = 32;

let NOrderByteHashMap = HashMap(26, 4, { prob: U24Max >> 1, count: 0 }, (view, value) => {
    view.setUint32(0, value.prob & U24Max | (value.count << 24));
}, (view) => {
    return {
        prob: view.getUint32(0) & U24Max,
        count: view.getUint32(0) >>> 24
    };
});

let NOrderByte = (byteMask, isWord) => {
    let ctx = 0;
    let maxCount = 15;
    let bitMask = 0n;
    let bitCtx = 1;
    let prevBytes = isWord ? 2166136261n : 0n;
    let magicNum = hash(isWord ? 1337n : BigInt(byteMask), 2);

    for (let i = 0; i < 8; i++) {
        bitMask |= BigInt((byteMask >>> i) & 1) * (BigInt(0xff) << BigInt(i * 8));
    }
    bitMask = isWord ? U64Max : bitMask;

    return {
        pred: () => {
            return probStretch(NOrderByteHashMap.get(ctx ^ bitCtx).prob / U24Max);
        },
        learn: (bit) => {
            let value = NOrderByteHashMap.get(ctx ^ bitCtx);
            if (value.count < maxCount) {
                value.count++;
            }
            let countSqrt = value.count + 0.2;
            value.prob += (U24Max * ((bit - (value.prob / U24Max)) / countSqrt)) | 0;
            NOrderByteHashMap.set(ctx ^ bitCtx, value);

            bitCtx = (bitCtx << 1) | bit;
            if (bitCtx >= 256) {
                let currentByte = bitCtx & 0xff;

                if (isWord) {
                    let nextChar = currentByte;
                    if ((nextChar >= 65 && nextChar <= 90) || (nextChar >= 97 && nextChar <= 122) || (nextChar >= 48 && nextChar <= 57)) {
                        // Make nextChar lowercase
                        if (nextChar >= 65 && nextChar <= 90)
                            nextChar ^= ASCII_CASE_MASK;
                        prevBytes = ((((prevBytes ^ BigInt(nextChar)) * 16777619n) & U64Max) >> 16n);
                    } else {
                        prevBytes = 2166136261n;
                    }
                } else {
                    prevBytes = ((prevBytes << 8n) | BigInt(currentByte)) & U64Max;
                }

                let maskedBytes = prevBytes & bitMask;
                ctx = Number((((hash(maskedBytes >> 32n, 3) * 9n + hash(maskedBytes, 3)) + 1n) * magicNum) & U32Max);

                bitCtx = 1;
            }
        },
    };
};