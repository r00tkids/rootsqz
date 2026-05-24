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

.globl _arithmetic_decode_stream
_arithmetic_decode_stream:
    stp     x29, x30, [sp, #-96]!
    mov     x29, sp
    stp     x19, x20, [sp, #16]
    stp     x21, x22, [sp, #32]
    stp     x23, x24, [sp, #48]
    stp     x25, x26, [sp, #64]
    stp     x27, x28, [sp, #80]

    mov     x19, x0                 // ctx
    mov     x20, x1                 // output
    mov     x21, x2                 // bytes remaining
    adrp    x22, _rootsqz_model_ctx@PAGE
    add     x22, x22, _rootsqz_model_ctx@PAGEOFF

    ldr     w25, [x19, #DEC_LOW]
    ldr     w26, [x19, #DEC_HIGH]
    ldr     w27, [x19, #DEC_STATE]
    ldr     x28, [x19, #DEC_INPUT]

1:
    cbz     x21, 5f
    mov     w23, #0                 // byte accumulator
    mov     w24, #8

2:
    mov     x0, x22
    bl      _rootsqz_model_predict  // d0 = squashed probability

    // _arithmetic_decode_bit inlined: x25=low, x26=high, x27=state, x28=input
    sub     w9, w26, w25            // range = high - low
    ucvtf   d1, w9
    ucvtf   d2, w25
    fmadd   d1, d1, d0, d2          // mid = range * p + low
    fcvtzu  w9, d1                  // Rust's f64 as u32 truncates toward zero

    cmp     w9, w26
    b.lo    3f
    sub     w9, w26, #1             // clamp when mid >= high
3:
    cmp     w27, w9                 // state vs mid
    b.hi    4f
    mov     w10, #1                 // bit = 1
    mov     w26, w9                 // high = mid
    b       6f
4:
    mov     w10, #0                 // bit = 0
    add     w25, w9, #1             // low = mid + 1

6:
    mov     w11, #TOP
7:
    eor     w12, w26, w25
    cmp     w12, w11
    b.hs    8f

    lsl     w25, w25, #8
    lsl     w26, w26, #8
    orr     w26, w26, #0xff

    ldr     x12, [x19, #DEC_INPUT_END]
    cmp     x28, x12
    b.hs    .Ldec_pad
    ldrb    w12, [x28], #1
    b       .Ldec_got_byte
.Ldec_pad:
    mov     w12, #0
.Ldec_got_byte:
    lsl     w27, w27, #8
    orr     w27, w27, w12
    b       7b

8:
    lsl     w23, w23, #1
    orr     w23, w23, w10

    mov     x0, x22
    mov     w1, w10
    bl      _rootsqz_model_learn

    subs    w24, w24, #1
    b.ne    2b

    strb    w23, [x20], #1
    subs    x21, x21, #1
    b       1b

5:
    str     w25, [x19, #DEC_LOW]
    str     w26, [x19, #DEC_HIGH]
    str     w27, [x19, #DEC_STATE]
    str     x28, [x19, #DEC_INPUT]

    ldp     x27, x28, [sp, #80]
    ldp     x25, x26, [sp, #64]
    ldp     x23, x24, [sp, #48]
    ldp     x21, x22, [sp, #32]
    ldp     x19, x20, [sp, #16]
    ldp     x29, x30, [sp], #96
    ret
