// The byte that most often followed the last `contextBytes` bytes
let IndirectByte = (contextBytes, maxCount, last = 0, transitions = new Uint16Array(1 << 8 * contextBytes)) => CounterModel(maxCount, byte => {
    let mask = transitions.length - 1, i = last & mask, candidate = transitions[i] & 255, count = transitions[i] >> 8;
    // A saturating vote keeps a stable candidate while allowing replacement
    transitions[i] = !count || count == 1 && candidate != byte ? 256 | byte
        : candidate == byte ? Math.min(count + 1, 255) << 8 | byte : count - 1 << 8 | candidate;
    return contextHash(0, transitions[(last = last << 8 | byte) & mask] & 255, hash(9191, 2));
});
