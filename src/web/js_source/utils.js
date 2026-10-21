let U24Max = 0xffffff;

// Computes bit for bit what prob_stretch and prob_squash in src/compressor/utils.rs compute,
// with +, -, * and / only: Math.log and Math.exp differ between engines in the last bit.
let probStretch = (p, e = 0, r = 0, i = 1, x, s, t) => {
    for (x = (p = Math.max(1 / U24Max, Math.min(1 - 1 / U24Max, p))) / (1 - p); x > 2; e++) x /= 2;
    for (; x < 1; e--) x *= 2;
    for (t = 2 * (s = (x - 1) / (x + 1)); i < 24; i += 2) r += t / i, t *= s * s;
    return r + e * Math.LN2;
};
let probSquash = (p, t = 1, r = 1) => {
    for (let i = 1; i < 24; i++) r = i < 14 ? r + (t *= -p / 1024 / i) : r * r;
    return 1 / (1 + r);
};
// The hash of the Rust encoder, on 32 bit unsigned integers
let hash = (value, shift) => Math.imul(value ^ value >>> shift, 0x9E35A7BD) >>> shift;
