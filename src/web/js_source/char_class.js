// The classes of the previous `order` bytes: word character, digit, space, quote, opening and
// closing bracket, operator, other
let CharClass = (order, maxCount, classes = 0) => CounterModel(maxCount, byte => contextHash(0,
    (classes = classes << 3 | [/[a-z_$]/i, /\d/, /[ \t\r\n]/, /['"`]/, /[([{]/, /[)\]}]/, /[.,:;+*=<>!&|%^~?/\\-]/]
        .findIndex(r => r.test(String.fromCharCode(byte))) + 1) & (1 << order * 3) - 1,
    hash(0x434c4153 ^ order, 2)));
