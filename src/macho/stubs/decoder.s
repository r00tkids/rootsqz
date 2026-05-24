// AArch64 arithmetic decoder for streams produced by src/compressor/encoder.rs.
//
// The core coder state is intentionally separate from the probability model.
// The Rust encoder computes:
//
//     p = prob_squash(model.pred())
//     coder.encode(bit, p)
//     model.learn(bit)
//
// This file mirrors the coder part exactly and decodes byte streams using the
// generated Mach-O model symbols.
//
// Decoder context layout:
//     struct ArithmeticDecoder {
//         uint32_t low;
//         uint32_t high;
//         uint32_t state;
//         uint32_t pad;
//         const uint8_t *input;
//         const uint8_t *input_end;
//     };
//
// Exported ABI:
//     void arithmetic_decoder_init(ctx, input, input_len)
//         x0 = ArithmeticDecoder *
//         x1 = encoded bytes
//         x2 = encoded byte length
//
//     uint32_t arithmetic_decode_bit(ctx, p)
//         x0 = ArithmeticDecoder *
//         d0 = probability that the next bit is 1, already squashed to [0, 1]
//         w0 = decoded bit
//
//     void arithmetic_decode_stream(ctx, output, output_len)
//         x0 = ArithmeticDecoder *
//         x1 = output bytes
//         x2 = output byte length

.text
.align 2

.equ DEC_LOW,       0
.equ DEC_HIGH,      4
.equ DEC_STATE,     8
.equ DEC_PAD,       12
.equ DEC_INPUT,     16
.equ DEC_INPUT_END, 24
.equ TOP,           0x01000000

.globl _arithmetic_decoder_init
_arithmetic_decoder_init:
    mov     x9, x0                  // ctx
    add     x10, x1, x2             // input_end
    mov     w11, #0                 // state
    mov     w12, #4

1:
    cmp     x1, x10
    b.hs    2f
    ldrb    w13, [x1], #1
    b       3f
2:
    mov     w13, #0
3:
    lsl     w11, w11, #8
    orr     w11, w11, w13
    subs    w12, w12, #1
    b.ne    1b

    mov     w13, #0
    str     w13, [x9, #DEC_LOW]
    mov     w13, #-1
    str     w13, [x9, #DEC_HIGH]
    str     w11, [x9, #DEC_STATE]
    mov     w13, #0
    str     w13, [x9, #DEC_PAD]
    str     x1, [x9, #DEC_INPUT]
    str     x10, [x9, #DEC_INPUT_END]
    ret

.globl _arithmetic_decode_bit
_arithmetic_decode_bit:
    ldr     w9, [x0, #DEC_LOW]      // low
    ldr     w10, [x0, #DEC_HIGH]    // high
    ldr     w11, [x0, #DEC_STATE]   // state
    ldr     x2, [x0, #DEC_INPUT]
    ldr     x3, [x0, #DEC_INPUT_END]

    sub     w12, w10, w9            // range = high - low
    ucvtf   d1, w12
    ucvtf   d2, w9
    fmadd   d1, d1, d0, d2          // mid = range * p + low
    fcvtzu  w12, d1                 // Rust's f64 as u32 truncates toward zero

    cmp     w12, w10
    b.lo    1f
    sub     w12, w10, #1            // clamp when mid >= high
1:
    cmp     w11, w12
    b.hi    2f
    mov     w13, #1                 // bit = 1
    mov     w10, w12                // high = mid
    b       3f
2:
    mov     w13, #0                 // bit = 0
    add     w9, w12, #1             // low = mid + 1

3:
    mov     w15, #TOP
4:
    eor     w14, w10, w9
    cmp     w14, w15
    b.hs    7f

    lsl     w9, w9, #8
    lsl     w10, w10, #8
    orr     w10, w10, #0xff

    cmp     x2, x3
    b.hs    5f
    ldrb    w14, [x2], #1
    b       6f
5:
    mov     w14, #0
6:
    lsl     w11, w11, #8
    orr     w11, w11, w14
    b       4b

7:
    str     w9, [x0, #DEC_LOW]
    str     w10, [x0, #DEC_HIGH]
    str     w11, [x0, #DEC_STATE]
    str     x2, [x0, #DEC_INPUT]
    mov     w0, w13
    ret

.globl _arithmetic_decode_stream
_arithmetic_decode_stream:
    stp     x29, x30, [sp, #-64]!
    stp     x19, x20, [sp, #16]
    stp     x21, x22, [sp, #32]
    stp     x23, x24, [sp, #48]

    mov     x19, x0                 // ctx
    mov     x20, x1                 // output
    mov     x21, x2                 // bytes remaining
    adrp    x22, _rootsqz_model_ctx@PAGE
    add     x22, x22, _rootsqz_model_ctx@PAGEOFF

1:
    cbz     x21, 5f
    mov     w23, #0                 // byte accumulator
    mov     w24, #8

2:
    mov     x0, x22
    bl      _rootsqz_model_predict

    mov     x0, x19
    bl      _arithmetic_decode_bit
    and     w9, w0, #1

    lsl     w23, w23, #1
    orr     w23, w23, w9

    mov     x0, x22
    mov     w1, w9
    bl      _rootsqz_model_learn

    subs    w24, w24, #1
    b.ne    2b

    strb    w23, [x20], #1
    subs    x21, x21, #1
    b       1b

5:
    ldp     x23, x24, [sp, #48]
    ldp     x21, x22, [sp, #32]
    ldp     x19, x20, [sp, #16]
    ldp     x29, x30, [sp], #64
    ret
