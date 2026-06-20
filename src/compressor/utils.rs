pub const U24_MAX: u32 = 0xffffff;

pub const PROB_EPS: f64 = 1.0 / U24_MAX as f64;
pub fn prob_stretch(prob: f64) -> f64 {
    let prob = prob.clamp(PROB_EPS, 1.0 - PROB_EPS);
    (prob / (1. - prob)).ln()
}

pub fn prob_squash(prob_stretched: f64) -> f64 {
    1. / (1. + f64::exp(-prob_stretched))
}
