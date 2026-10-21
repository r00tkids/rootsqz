// Mixes the inputs of the models, a number or an array each (see Model::pred_inputs), with weights
// learned for all bits, plus weights learned per previous byte and bits of this byte so far
let Mixer = (models, learningRate, contextLearningRate, contextWeightScale) => {
    let weights, contextWeights = [], preds = [], prev = 0, bitCtx = 1, total;
    return {
        p: (sum = 0, w = contextWeights[prev << 8 | bitCtx]) => (
            preds = models.flatMap(m => m.p()),
            weights ||= preds.map(() => 1 / preds.length),
            preds.map((p, i) => sum += p * (w ? w[i] * contextWeightScale + weights[i] : weights[i])),
            total = probSquash(sum),
            sum),
        l: (bit, w = contextWeights[prev << 8 | bitCtx] ||= [...weights], err = bit - total) => {
            models.map(m => m.l(bit));
            preds.map((p, i) => (weights[i] += learningRate * err * p, w[i] += contextLearningRate * err * p));
            (bitCtx = bitCtx << 1 | bit) > 255 && (prev = bitCtx & 255, bitCtx = 1);
        }
    };
};
