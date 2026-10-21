// The bit counters of the context models, shared by all of them, as ContextTable in context_map.rs.
// A slot is two words: a probability in the low 24 bits and a count in the high 8, and the check
// of the key of its context (12 bits), run count (4 bits), bit history state (8 bits) and run byte
// (8 bits). `counters` and `counterBits` are generated with the table size.

// The context of 8 masked bytes, as the high and low 32 bits, for a model with the hash `magic`
let contextHash = (hi, lo, magic) => Math.imul(hash(hi, 3) * 9 + hash(lo, 3) + 1, magic);

// Moves the counter at a[i] towards bit, by less the more often it has learnt, up to max times
let learnCounter = (a, i, bit, max, count = a[i] >>> 24, prob = a[i] & U24Max) => (
    count += count < max,
    a[i] = prob + (U24Max * ((bit - prob / U24Max) / (count + 0.2)) | 0) & U24Max | count << 24);

// A model of the counter at the key of its context and the bits of the byte so far, which gives the
// mixer the inputs of ContextStats in context_map.rs. `next` computes the context from each byte; a
// context of null predicts nothing and learns nothing. A key with another check than its slot takes
// the slot over.
let CounterModel = (maxCount, next, ctx = 0, key = bitCtx => ctx ^ bitCtx, bitCtx = 1,
    i, state, run, runInfo, states = new Uint32Array(65536).fill(U24Max >> 1)) => ({
    p: (k = key(bitCtx), check = Math.imul(k, 0x2545F491) >>> 20 | 1, bits = 31 - Math.clz32(bitCtx), p, info) => ctx == null ? [0, 0, 0, 0, 0] : (
        i = (Math.imul(k ^ k >>> 15, 0x2C1B3C6D) >>> 32 - counterBits) * 2,
        counters[i + 1] >>> 20 != check && (counters[i] = U24Max >> 1, counters[i + 1] = check << 20),
        info = counters[i + 1],
        // The run of the byte, kept in the slot of its first bit
        bitCtx < 2 && (run = i, runInfo = info),
        state = (info >>> 8 & 255) << 8 | bitCtx,
        p = (counters[i] & U24Max) / U24Max,
        [
            probStretch(p) * 0.5,
            (p - 0.5) * 3,
            probStretch((states[state] & U24Max) / U24Max) * 0.4,
            (info = runInfo >>> 16 & 15) && (runInfo & 255 | 256) >> 8 - bits == bitCtx
                ? (runInfo >> 7 - bits & 1 ? info : -info) * 0.25 : 0,
            counters[i] >>> 24 ? 0 : 1
        ]),
    l: (bit, s) => {
        if (ctx != null) {
            learnCounter(counters, i, bit, maxCount);
            learnCounter(states, state, bit, 127);
            s = (counters[i + 1] >>> 8 & 255 || 1) * 2 | bit;
            counters[i + 1] = counters[i + 1] & ~0xff00 | (s > 255 ? s & 127 | 128 : s) << 8;
        }
        if ((bitCtx = bitCtx << 1 | bit) > 255) {
            s = counters[run + 1];
            ctx != null && s >>> 20 == runInfo >>> 20 &&
                (counters[run + 1] = s & 0xfff0ff00 | ((runInfo & 255) == (bitCtx & 255) && Math.min(runInfo >>> 16 & 15, 14)) + 1 << 16 | bitCtx & 255);
            ctx = next(bitCtx & 255), bitCtx = 1;
        }
    }
});
