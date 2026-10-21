// Mirrors ContextMixer and Apm in mixer.rs. The models may give several inputs (i), a state (s)
// and a context (c). selectors: 0 one weight set, 1 order 1, 2 the number of models that know
// their context, 100 + i the context of model i, 200 + i the state of model i.
// apms: [context, model, limit, weight, bits of the hash], with the contexts 0 order 0, 1 order 1,
// 2 order 2, 3 the state of the model, 4 the context of the model
let ContextMixer = (models, selectors, learningRate, finalLearningRate, initWeight, lrBoost, apms) => {
    let k = selectors.length, weights = selectors.map(() => []),
        finalSet = Array(k).fill(1 / k), states = [], contexts = [], pMixer, inputs, sets, dots, finalDot, bitCtx = 1, history = 0;
    apms = apms.map(([context, model, limit, weight, bits]) => {
        let probs = new Float64Array(17 << (context == 3 ? 14 : context > 1 ? 8 + bits : 8 << context))
                .map((_, i) => probSquash(i % 17 - 8)),
            n = new Uint32Array(probs.length), index, frac;
        return {
            p: st => {
                let x = Math.min(Math.max(st, -8), 7.999) + 8, bucket = Math.floor(x);
                frac = x - bucket;
                index = ((context == 3 ? Math.min(states[model], 63)
                    : context > 1 ? Math.imul(context > 2 ? contexts[model] : history & 65535, 0x9E35A7BD) >>> 32 - bits
                    : context && history & 255) << 8 | bitCtx) * 17 + bucket;
                return (probs[index] * (1 - frac) + probs[index + 1] * frac - pMixer) * weight;
            },
            l: bit => [[index, 1 - frac], [index + 1, frac]].map(([i, w]) => {
                n[i] = Math.min(n[i] + 1, limit);
                probs[i] += (bit - probs[i]) * w / (n[i] + 0.5);
            })
        };
    });
    return {
        p: () => {
            let known = 0, st;
            inputs = [];
            models.map((model, i) => {
                model.i ? model.i(inputs) : inputs.push(...[].concat(model.p()));
                known += (states[i] = model.s?.() | 0) > 0;
                contexts[i] = model.c?.();
            });
            inputs.push(0.5);
            finalDot = 0;
            dots = [];
            sets = selectors.map((selector, s) => {
                let ctx = selector > 199 ? Math.min(states[selector - 200], 255) << 8 | bitCtx
                        : selector > 99 ? contexts[selector - 100] << 8 | bitCtx
                        : selector > 1 ? known * 8 + 31 - Math.clz32(bitCtx) : selector && (history & 255) << 8 | bitCtx,
                    w = weights[s][ctx] ||= Array(inputs.length).fill(initWeight), dot = 0;
                for (let i = 0; i < inputs.length; i++) dot += w[i] * inputs[i];
                dots[s] = dot = Math.min(Math.max(dot, -40), 40);
                finalDot += dot * finalSet[s];
                return w;
            });
            if (!apms.length) return finalDot;
            st = pMixer = probSquash(finalDot);
            for (let apm of apms) st += apm.p(finalDot);
            return probStretch(st);
        },
        l: bit => {
            sets.map((w, s) => {
                // w.n counts how many times the set was trained
                let count = w.n | 0, err = (bit - probSquash(dots[s])) * (learningRate * (1 + lrBoost * 16 / (16 + count)));
                w.n = count + 1;
                for (let i = 0; i < inputs.length; i++) w[i] += err * inputs[i];
            });
            let err = (bit - probSquash(finalDot)) * finalLearningRate;
            for (let s = 0; s < k; s++) finalSet[s] += err * dots[s];
            for (let model of models) model.l(bit);
            for (let apm of apms) apm.l(bit);
            bitCtx = bitCtx << 1 | bit;
            if (bitCtx > 255) {
                history = history << 8 | bitCtx & 255;
                bitCtx = 1;
            }
        }
    };
};
