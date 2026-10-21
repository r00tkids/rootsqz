// flags: 1 learns the trust per expected byte, 2 takes the second candidate (see MatchModel in model.rs)
let MatchModel = (minLen, candidates, flags) => {
    let history = [], head = new Int32Array(1 << 22), chain = [0],
        probs = new Int32Array(72 << 16).fill(U24Max >> 1), counts = new Uint8Array(72 << 16),
        matchPtr = 0, active = 0, len = 0, misses = 0, byteMissed = 0, partial = 1, i = 0, expectedBit = 0, state;
    return {
        p: (expectedByte = history[matchPtr], log = 31 - Math.clz32(len)) => {
            if (!active || byteMissed) return state = 0;
            state = 1 + (expectedBit = expectedByte >> Math.clz32(partial) - 24 & 1) + 2 * (len && Math.min(log + 1, 12));
            i = ((!len ? 63 + misses : len < 16 ? len : 16 + (log - 4) * 4 + (len >> log - 2 & 3)) << 8
                | (flags & 1 && expectedByte)) << 8 | partial;
            let p = probStretch(probs[i] / U24Max);
            return expectedBit ? p : -p;
        },
        // The prediction, and the expected bit as -1 or 1 times the length bucket of the state
        i(out) {
            out.push(this.p(), (expectedBit ? 1 : -1) * (state && state - 1 >> 1));
        },
        // 0 if the model predicts nothing for the bit, else 1 + the expected bit + 2 times a bucket of the length
        s: () => state,
        l: (bit, hit = bit == expectedBit) => {
            if (active && !byteMissed) {
                let count = counts[i] = Math.min(counts[i] + 1, 255);
                probs[i] += U24Max * ((hit - probs[i] / U24Max) / (count + 0.2)) | 0;
                hit || (len = 0, ++misses < 8 ? byteMissed = 1 : active = 0);
            }
            if ((partial = partial << 1 | bit) > 255) {
                let pos = history.push(partial & 255), hash = 0, slot;
                partial = 1;
                byteMissed = 0;
                active && (len && (len = Math.min(len + 1, 65535)), matchPtr++);
                if (pos < minLen) return chain.push(0);
                for (let i = pos - minLen; i < pos;) hash = Math.imul(hash ^ history[i++], 0x2F0F3D1) + 1;
                slot = hash >>> 8 & 0x3FFFFF;
                if (!len) {
                    let candidate = head[slot], bestLen = 0, bestPtr = 0, firstByte = -1;
                    for (let n = 0; n < candidates && candidate; ++n) {
                        let l = 0;
                        while (l < 65535 && l < candidate && history[candidate - 1 - l] == history[pos - 1 - l]) l++;
                        if (flags & 2) {
                            if (l >= minLen) {
                                if (firstByte < 0) firstByte = history[candidate];
                                else if (firstByte != history[candidate]) {
                                    bestLen = l;
                                    bestPtr = candidate;
                                    break;
                                }
                            }
                        } else if (l > bestLen) bestLen = l, bestPtr = candidate;
                        candidate = chain[candidate];
                    }
                    bestLen >= minLen && (len = bestLen, matchPtr = bestPtr, active = 1, misses = 0);
                }
                chain.push(head[slot]);
                head[slot] = pos;
            }
        }
    };
};
