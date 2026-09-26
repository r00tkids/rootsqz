let TokenPosition = (maxCount, contextBytes, maxPosition) => {
    let kind = 0, position = 0, delimiter = 0, prevBytes = 0, escaped = false, bitCtx = 1, ctx = 0;
    return {
        pred: () => probStretch(NOrderByteHashMap.get(ctx ^ bitCtx).prob / U24Max),
        learn: bit => {
            let value = NOrderByteHashMap.get(ctx ^ bitCtx);
            value.count = Math.min(value.count + 1, maxCount);
            value.prob += (U24Max * ((bit - value.prob / U24Max) / (value.count + 0.2))) | 0;
            NOrderByteHashMap.set(ctx ^ bitCtx, value);
            bitCtx = (bitCtx << 1) | bit;
            if (bitCtx >= 256) {
                let byte = bitCtx & 255;
                if (kind >= 3) {
                    let quote = kind === 3 ? 39 : kind === 4 ? 34 : 96;
                    if (escaped) {
                        escaped = false;
                        position = Math.min(position + 1, maxPosition);
                    } else if (byte === 92) {
                        escaped = true;
                        position = Math.min(position + 1, maxPosition);
                    } else if (byte === quote) {
                        kind = 0;
                        position = 0;
                        delimiter = byte;
                    } else position = Math.min(position + 1, maxPosition);
                } else if (byte === 39 || byte === 34 || byte === 96) {
                    kind = byte === 39 ? 3 : byte === 34 ? 4 : 5;
                    position = 0;
                } else if ((byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || byte === 95 || byte === 36 || (kind === 1 && byte >= 48 && byte <= 57)) {
                    position = kind === 1 ? Math.min(position + 1, maxPosition) : 1;
                    kind = 1;
                } else if (byte >= 48 && byte <= 57) {
                    position = kind === 2 ? Math.min(position + 1, maxPosition) : 1;
                    kind = 2;
                } else {
                    kind = 0;
                    position = 0;
                    if (byte !== 9 && byte !== 10 && byte !== 12 && byte !== 13 && byte !== 32) delimiter = byte;
                }
                prevBytes = ((prevBytes << 8) | byte) & 65535;
                let recent = contextBytes === 0 ? 0 : contextBytes === 1 ? prevBytes & 255 : prevBytes;
                let state = kind | (position << 3) | (delimiter << 11);
                ctx = Math.imul(Number(hash(BigInt((state ^ (recent << 19) ^ 0x6b8b4567) >>> 0), 3)), 0x85ebca6b);
                bitCtx = 1;
            }
        }
    };
};
