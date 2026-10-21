// Predicts bits in the record ranges, which Records lists as the gap before each range and its length, in
// blocks of 512 bytes, and its stride. It predicts from the bytes at dists: bytes back if positive, records
// back if negative. With column, also from the position in the record. With delta, from the byte that
// continues the words of delta bytes (|delta| bytes, linearly if positive) at the same place in the last
// records. Its counter predicts whether a bit is the one of the expected byte, which is 255 without delta,
// so it learns whether each bit was, like RecordModel in records.rs.
let RecordModel = (dists, column, delta, maxCount) => {
    let history = [], rangeIdx = 0, start, end = 0, stride, expected, bits = 0,
        mix = (h, v) => (h = Math.imul(h ^ v, 0x9E3779B1), h << 13 | h >>> 19),
        magic = dists.reduce(mix, mix(delta, column)),
        word = (at, w, v = 0) => {
            while (w--) v = v * 256 + history[at + w];
            return v;
        },
        // The context of the next byte, or null outside the ranges
        next = (pos = history.length, h = magic) => {
            while (pos >= end && rangeIdx < Records.length)
                start = end + Records[rangeIdx++] * 512, end = start + Records[rangeIdx++] * 512, stride = Records[rangeIdx++];
            if (pos < start || pos >= end) return null;
            if (column) h = mix(h, (pos - start) % stride);
            for (let d of dists) h = mix(h, (d = d > 0 ? d : -d * stride) <= pos ? history[pos - d] : 256);
            expected = 255;
            if (delta) {
                let w = delta < 0 ? -delta : delta, rec = pos - (pos - start) % stride, ws = pos - (pos - rec) % w, k = pos - ws;
                if (ws + w > rec + stride || ws < start + stride * (1 + (delta > 0))) return null;
                let m = 2 ** (8 * w), km = 2 ** (8 * k), a = word(ws - stride, w),
                    p = delta > 0 ? (2 * a + m - word(ws - 2 * stride, w)) % m : a,
                    d = (word(ws, k) + km - p % km) % km;
                if (k && d >= km / 2) d += m - km;
                expected = Math.floor((p + d) % m / km) & 255;
                h = mix(h, k);
            }
            return mix(h, h >>> 15);
        },
        // Learns whether each bit was the expected one, so the byte is those bits xor the expected byte, inverted
        counter = CounterModel(maxCount, byte => (history.push(byte ^ expected ^ 255), next()), next()),
        bit1 = () => expected >> 7 - bits & 1;
    return {
        p: () => counter.p().map(p => bit1() ? p : -p),
        // With a ContextMixer: the count of the counter
        s: () => counter.s(),
        l: bit => {
            counter.l(bit ^ bit1() ^ 1);
            bits = bits + 1 & 7;
        }
    };
};
