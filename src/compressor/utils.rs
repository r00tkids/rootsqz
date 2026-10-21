pub const U24_MAX: u32 = 0xffffff;

pub const PROB_EPS: f64 = 1.0 / U24_MAX as f64;

// The JS decoder (src/web/js_source/utils.js) must compute bit for bit the same predictions, or
// the arithmetic decoder goes off track. So stretch and squash use only +, -, * and /, which IEEE 754
// rounds the same everywhere, in the same order as the JS: f64::ln, f64::exp and Math.log, Math.exp
// come from different math libraries that differ in the last bit, and Rust must not fuse a * b + c.

pub fn prob_stretch(prob: f64) -> f64 {
    let prob = prob.clamp(PROB_EPS, 1.0 - PROB_EPS);
    // ln(x) = e ln 2 + 2 atanh((m - 1) / (m + 1)), with x = m 2^e and m in [1, 2]
    let mut x = prob / (1. - prob);
    let mut e = 0;
    while x > 2. {
        x /= 2.;
        e += 1;
    }
    while x < 1. {
        x *= 2.;
        e -= 1;
    }
    let s = (x - 1.) / (x + 1.);
    let mut t = 2. * s;
    let mut r = 0.;
    for i in (1..24).step_by(2) {
        r += t / i as f64;
        t *= s * s;
    }
    r + e as f64 * std::f64::consts::LN_2
}

pub fn prob_squash(prob_stretched: f64) -> f64 {
    // exp(-p) = exp(-p / 1024)^1024, with the Taylor series of exp(-p / 1024)
    let mut t = 1.;
    let mut r = 1.;
    for i in 1..14 {
        t *= -prob_stretched / 1024. / i as f64;
        r += t;
    }
    for _ in 0..10 {
        r *= r;
    }
    1. / (1. + r)
}
