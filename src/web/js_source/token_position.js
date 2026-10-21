// The kind of the lexical token, the position in it and the last delimiter
let TokenPosition = (maxCount, contextBytes, maxPosition, kind = 0, position = 0, delimiter = 0, last = 0, escaped = 0) => CounterModel(maxCount, byte => {
    let char = String.fromCharCode(byte), step = () => position = Math.min(position + 1, maxPosition), quote = `'"\``.indexOf(char);
    if (kind > 2)
        escaped ? (escaped = 0, step()) : byte == 92 ? (escaped = 1, step()) : quote == kind - 3 ? (kind = position = 0, delimiter = byte) : step();
    else if (quote >= 0)
        kind = quote + 3, position = 0;
    else if (/[a-z_$]/i.test(char) || kind == 1 && /\d/.test(char))
        position = kind == 1 ? step() : 1, kind = 1;
    else if (/\d/.test(char))
        position = kind == 2 ? step() : 1, kind = 2;
    else
        kind = position = 0, /[\t\n\f\r ]/.test(char) || (delimiter = byte);
    last = (last << 8 | byte) & 65535;
    return Math.imul(hash(((kind | position << 3 | delimiter << 11) ^ (contextBytes > 1 ? last : contextBytes ? last & 255 : 0) << 19 ^ 0x6b8b4567) >>> 0, 3), 0x85ebca6b);
});
