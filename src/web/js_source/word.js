// The hash of the word so far: letters (in lower case), digits and _.[]
let Word = (maxCount, word = 2166136261) => CounterModel(maxCount, byte => (
    byte = String.fromCharCode(byte),
    contextHash(0, word = /[\w.[\]]/.test(byte) ? Math.imul(word ^ byte.toLowerCase().charCodeAt(), 16777619) : 2166136261, hash(1337, 2))));
