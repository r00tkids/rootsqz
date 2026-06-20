let LnMixerPred = (models, learningRate = 0.0004, contextLearningRate = 0.022, contextWeightScale = 0.3) => {
    models = models.map(model => ({
        model: model,
        weight: 1 / models.length
    }));
    let lastTotalP = 0;
    let lastP = new Array(models.length);
    let weights = new Array(256);
    for (let i = 0;i < 256;++i) {
        let a = new Array(255);
        weights[i] = a;
    }

    let bitCtx = 1;
    let ctx = 0;

    return {
        pred: () => {
            let sum = 0;
            let weightsForCtx = weights[ctx][bitCtx - 1];
            for (let i = 0;i < models.length;++i) {
                let weight = weightsForCtx ? weightsForCtx[i] * contextWeightScale + models[i].weight : models[i].weight;

                let p = models[i].model.pred();
                lastP[i] = p;
                sum += p * weight;
            }

            lastTotalP = probSquash(sum);
            return sum;
        },
        learn: (bit) => {
            let weightsForCtx = weights[ctx][bitCtx - 1];
            if (!weightsForCtx) {
                weights[ctx][bitCtx - 1] = weightsForCtx = new Array(models.length);
                for (let i = 0;i < models.length;++i) {
                    weightsForCtx[i] = models[i].weight;
                }
            }

            let predErr = bit - lastTotalP;
            for (let i = 0;i < models.length;++i) {
                models[i].model.learn(bit);
                models[i].weight += learningRate * predErr * lastP[i];
                weightsForCtx[i] += contextLearningRate * predErr * lastP[i];
            }

            bitCtx = (bitCtx << 1) | bit;
            if (bitCtx >= 256) {
                ctx = bitCtx & 0xFF;
                bitCtx = 1;
            }
        },
    };
};
