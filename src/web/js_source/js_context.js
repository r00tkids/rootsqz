// Mirrors JsContext and JsLexer in model.rs
let JsContext = (variant, maxCount) => {
    let quote = 0, kind = 0, token = 0, prevToken = 0, punct = 0, skeletonLo = 0, skeletonHi = 0,
        frames = [[0, 0]], nextToken = {}, textRun = 0,
        mix = (a, b) => Math.imul(a, 0x9E3779B1) + b | 0,
        addToSkeleton = x => (skeletonHi = skeletonHi << 8 | skeletonLo >>> 24, skeletonLo = skeletonLo << 8 | x),
        endToken = () => kind && (nextToken[prevToken] = token, prevToken = token, addToSkeleton(kind), kind = 0);
    // Bytes since the last control byte below tab, up to 32: the context is null until 32
    return Object.assign(CounterModel(maxCount, c => {
        textRun = c < 9 ? 0 : textRun + 1;
        if (quote) c == quote ? (quote = 0, endToken()) : token = mix(token, c);
        else if (/[\w$]/.test(String.fromCharCode(c)) || c == 46 && kind == 2) kind || (token = kind = c > 47 && c < 58 ? 2 : 1), token = mix(token, c);
        else {
            endToken();
            if (c == 34 || c == 39 || c == 96) quote = c, token = kind = 3;
            else if (c != 32 && c != 9 && c != 10 && c != 13) {
                punct = c;
                addToSkeleton(c);
                if (c == 40 || c == 91 || c == 123) frames.push([c, 0]);
                else if ((c == 41 || c == 93 || c == 125) && frames.length > 1) frames.pop();
                else if (c == 58) frames.at(-1)[1] = prevToken;
            }
        }
        let [open, key] = frames.at(-1), t = kind ? token : 0,
            h = mix(variant == 0 ? mix(mix(mix(t, open), key), punct)
                : variant == 1 ? mix(t, nextToken[prevToken] | 0)
                : variant == 2 ? mix(mix(t, c), open << 8 | frames.length)
                : mix(mix(t, skeletonLo), skeletonHi), variant);
        return textRun < 32 ? null : Math.imul(h ^ h >>> 15, 0x85EBCA6B) & ~255;
    }, null), {
        // The bracket, the class of the token and the last punctuation, see JsContext::context
        c: () => ([40, 91, 123].indexOf(frames.at(-1)[0]) + 1 << 2 | kind) << 8 | punct
    });
};
