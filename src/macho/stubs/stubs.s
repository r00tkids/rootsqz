// AArch64 Mach-O decompressor stubs.
//
// Merged from bootstrap.s, decoder.s, model_support.s, norder_byte.s,
// word.s, and ln_mixer.s so that inlining and size optimisation can be
// applied across the whole stub.
//
// tiny_runtime.s is kept separate because diagnostics mode substitutes
// diagnostic_runtime.c for those two symbols.

// ============================================================
// bootstrap
// ============================================================

.text
.align 2

.extern _arithmetic_decode_stream

.extern _rootsqz_compressed_start
.extern _rootsqz_compressed_end
.extern _rootsqz_decode_chunks_start
.extern _rootsqz_decode_chunks_end
.extern _rootsqz_prepare_image
.extern _rootsqz_launch_image

.private_extern _main
_main:
    stp     x29, x30, [sp, #-16]!
    mov     x21, x0                 // argc
    mov     x22, x1                 // argv
    mov     x23, x2                 // envp

    // ArithmeticDecoder context, matching decoder.s' 32-byte layout.
    adrp    x1, _rootsqz_compressed_start@PAGE
    add     x1, x1, _rootsqz_compressed_start@PAGEOFF
    adrp    x2, _rootsqz_compressed_end@PAGE
    add     x2, x2, _rootsqz_compressed_end@PAGEOFF
    sub     x2, x2, x1
    add     x20, sp, #96
    // inlined arithmetic_decoder_init(ctx=x20, input=x1, input_len=x2):
    add     x10, x1, x2             // input_end
    mov     w11, #0                 // state
    mov     w12, #4
.Ldinit_loop:
    cmp     x1, x10
    b.hs    .Ldinit_pad
    ldrb    w13, [x1], #1
    b       .Ldinit_got_byte
.Ldinit_pad:
    mov     w13, #0
.Ldinit_got_byte:
    lsl     w11, w11, #8
    orr     w11, w11, w13
    subs    w12, w12, #1
    b.ne    .Ldinit_loop
    mov     w13, #0
    str     w13, [x20, #0]          // DEC_LOW
    mov     w13, #-1
    str     w13, [x20, #4]          // DEC_HIGH
    str     w11, [x20, #8]          // DEC_STATE
    mov     w13, #0
    str     w13, [x20, #12]         // DEC_PAD
    str     x1, [x20, #16]          // DEC_INPUT
    str     x10, [x20, #24]         // DEC_INPUT_END

    bl      _rootsqz_prepare_image
    mov     x19, x0                 // mapped app image

    adrp    x24, _rootsqz_decode_chunks_start@PAGE
    add     x24, x24, _rootsqz_decode_chunks_start@PAGEOFF
    adrp    x25, _rootsqz_decode_chunks_end@PAGE
    add     x25, x25, _rootsqz_decode_chunks_end@PAGEOFF

1:
    cmp     x24, x25
    b.hs    2f

    ldr     x1, [x24], #8           // chunk image offset
    ldr     x2, [x24], #8           // chunk byte length
    add     x1, x19, x1
    mov     x0, x20
    bl      _arithmetic_decode_stream
    b       1b

2:
    mov     x0, x19
    mov     w1, w21
    mov     x2, x22
    mov     x3, x23
    bl      _rootsqz_launch_image

    ldp     x29, x30, [sp], #16
    ret

// ============================================================
// decoder
// ============================================================

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


.private_extern _arithmetic_decode_stream
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

// ============================================================
// model_support
// ============================================================

// Shared AArch64 Mach-O helpers for assembly probability models.
//
// Model entrypoints that mirror Rust's Model::pred return stretched
// probabilities.  The final _rootsqz_model_predict callback used by decoder.s
// must squash that value to [0, 1] before returning.

.text
.align 2

.equ rootsqz_U24_MAX, 0x00ffffff
.equ rootsqz_NORDER_DATA_DEFAULT, 0x007fffff


.private_extern _rootsqz_prob_squash
_rootsqz_prob_squash:
    // d0 = 1.0 / (1.0 + exp(-d0))
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp

    fneg    d0, d0
    bl      _exp
    fmov    d1, #1.00000000
    fadd    d0, d0, d1
    fdiv    d0, d1, d0

    ldp     x29, x30, [sp], #16
    ret

.section __TEXT,__literal8,8byte_literals
.p2align 3
.private_extern _rootsqz_u24_max_double
_rootsqz_u24_max_double:
    .double 16777215.0

// ============================================================
// norder_byte
// ============================================================

// AArch64 implementation of src/compressor/model.rs NOrderByte.
//
// Context layout:
//     uint32_t ctx;
//     uint32_t bit_ctx;
//     uint32_t magic_num;
//     uint32_t max_count;
//     uint64_t prev_bytes;
//     uint64_t mask;
//     uint32_t is_word_model;
//     uint32_t pad;
//     uint32_t *hash_table;
//     uint64_t hash_mask;
//
// Hash table records use NOrderByteData's packed layout:
//     bits 31..24: count
//     bits 23..0 : probability scaled by 0x00ffffff

.text
.align 2

// MODEL_HASH dst, src, tmp
// dst = (0x9E35A7BD * (src ^ (src >> 3))) >> 3
.macro MODEL_HASH dst, src, tmp
    lsr     \tmp, \src, #3
    eor     \dst, \src, \tmp
    movz    \tmp, #0xa7bd
    movk    \tmp, #0x9e35, lsl #16
    mul     \dst, \dst, \tmp
    lsr     \dst, \dst, #3
.endmacro

.equ NOB_CTX,        0
.equ NOB_BIT_CTX,    4
.equ NOB_MAGIC_NUM,  8
.equ NOB_MAX_COUNT, 12
.equ NOB_PREV_BYTES,16
.equ NOB_MASK,      24
.equ NOB_IS_WORD,   32
.equ NOB_TABLE,     40
.equ NOB_HASH_MASK, 48
.equ NOB_SIZE,      56

.private_extern _rootsqz_norder_byte_predict
_rootsqz_norder_byte_predict:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp
    ldr     w9, [x0, #NOB_CTX]
    ldr     w10, [x0, #NOB_BIT_CTX]
    eor     w9, w9, w10
    ldr     x10, [x0, #NOB_HASH_MASK]
    and     x9, x9, x10
    ldr     x10, [x0, #NOB_TABLE]
    ldr     w0, [x10, x9, lsl #2]
    cbnz    w0, 1f
    movz    w0, #0xffff
    movk    w0, #0x007f, lsl #16
1:
    movz    w9, #0xffff
    movk    w9, #0x00ff, lsl #16
    and     w0, w0, w9              // w9 = 0x00ffffff, reused below
    // inlined prob_stretch_u24(probability_from_u24(w0)): d0 = log(p/(1-p))
    cbnz    w0, 2f
    mov     w0, #1
    b       3f
2:
    cmp     w0, w9
    b.ne    3f
    sub     w0, w9, #1
3:
    adrp    x9, _rootsqz_u24_max_double@PAGE
    add     x9, x9, _rootsqz_u24_max_double@PAGEOFF
    ucvtf   d0, w0
    ldr     d1, [x9]
    fdiv    d0, d0, d1
    fmov    d2, d0
    fmov    d1, #1.00000000
    fsub    d1, d1, d2
    fdiv    d0, d2, d1
    bl      _log
    ldp     x29, x30, [sp], #16
    ret

.private_extern _rootsqz_norder_byte_learn
_rootsqz_norder_byte_learn:
    stp     x29, x30, [sp, #-80]!
    mov     x29, sp
    stp     x19, x20, [sp, #16]
    stp     x21, x22, [sp, #32]
    stp     x23, x24, [sp, #48]
    stp     x25, x26, [sp, #64]

    mov     x19, x0
    and     w20, w1, #1

    ldr     w9, [x19, #NOB_CTX]
    ldr     w10, [x19, #NOB_BIT_CTX]
    eor     w9, w9, w10
    ldr     x10, [x19, #NOB_HASH_MASK]
    and     x9, x9, x10
    ldr     x10, [x19, #NOB_TABLE]
    add     x21, x10, x9, lsl #2

    ldr     w22, [x21]              // packed NOrderByteData
    cbnz    w22, 9f
    movz    w22, #0xffff
    movk    w22, #0x007f, lsl #16
9:
    lsr     w23, w22, #24           // count
    movz    w24, #0xffff
    movk    w24, #0x00ff, lsl #16
    and     w25, w22, w24           // prob

    ldr     w9, [x19, #NOB_MAX_COUNT]
    cmp     w23, w9
    b.hs    1f
    add     w23, w23, #1
1:
    // prob += (U24_MAX * ((bit - prob/U24_MAX) / (count + 0.2))) as i32
    ucvtf   d0, w20
    ucvtf   d1, w25
    adrp    x9, _rootsqz_u24_max_double@PAGE
    add     x9, x9, _rootsqz_u24_max_double@PAGEOFF
    ldr     d2, [x9]
    fdiv    d1, d1, d2
    fsub    d0, d0, d1
    ucvtf   d3, w23
    adrp    x9, L_rootsqz_norder_learning_bias@PAGE
    add     x9, x9, L_rootsqz_norder_learning_bias@PAGEOFF
    ldr     d4, [x9]
    fadd    d3, d3, d4
    fdiv    d0, d0, d3
    fmul    d0, d0, d2
    fcvtzs  w9, d0
    add     w25, w25, w9
    and     w25, w25, w24
    orr     w9, w25, w23, lsl #24
    str     w9, [x21]

    ldr     w9, [x19, #NOB_BIT_CTX]
    lsl     w9, w9, #1
    orr     w9, w9, w20
    cmp     w9, #256
    b.hs    2f
    str     w9, [x19, #NOB_BIT_CTX]
    b       8f

2:
    and     w21, w9, #0xff          // current byte
    ldr     w9, [x19, #NOB_IS_WORD]
    cbnz    w9, 3f

    ldr     x22, [x19, #NOB_PREV_BYTES]
    lsl     x22, x22, #8
    orr     x22, x22, x21
    str     x22, [x19, #NOB_PREV_BYTES]
    b       6f

3:
    // ASCII alphanumeric test, with uppercase folded to lowercase.
    mov     w22, w21
    cmp     w22, #'0'
    b.lo    5f
    cmp     w22, #'9'
    b.ls    4f
    cmp     w22, #'A'
    b.lo    5f
    cmp     w22, #'Z'
    b.ls    7f
    cmp     w22, #'a'
    b.lo    5f
    cmp     w22, #'z'
    b.hi    5f
    b       4f
7:
    orr     w22, w22, #0x20
4:
    ldr     x23, [x19, #NOB_PREV_BYTES]
    eor     x23, x23, x22
    movz    w24, #0x0193
    movk    w24, #0x0100, lsl #16
    mul     x23, x23, x24
    lsr     x23, x23, #16
    str     x23, [x19, #NOB_PREV_BYTES]
    mov     x22, x23
    b       6f
5:
    movz    w22, #0x9dc5
    movk    w22, #0x811c, lsl #16
    str     x22, [x19, #NOB_PREV_BYTES]

6:
    ldr     x23, [x19, #NOB_MASK]
    and     x22, x22, x23

    lsr     x9, x22, #32
    MODEL_HASH w23, w9, w10

    MODEL_HASH w24, w22, w10

    add     w25, w23, w23, lsl #3
    add     w25, w25, w24
    add     w25, w25, #1
    ldr     w26, [x19, #NOB_MAGIC_NUM]
    mul     w25, w25, w26
    str     w25, [x19, #NOB_CTX]

    mov     w9, #1
    str     w9, [x19, #NOB_BIT_CTX]

8:
    ldp     x25, x26, [sp, #64]
    ldp     x23, x24, [sp, #48]
    ldp     x21, x22, [sp, #32]
    ldp     x19, x20, [sp, #16]
    ldp     x29, x30, [sp], #80
    ret

.section __TEXT,__literal8,8byte_literals
.p2align 3
L_rootsqz_norder_learning_bias:
    .double 0.2

// ============================================================
// word
// ============================================================

// Thin exported aliases for NOrderByte's word-model mode.
//
// Use the same context layout as norder_byte.s, with:
//     prev_bytes    = 2166136261
//     mask          = UINT64_MAX
//     is_word_model = 1
//     magic_num     = hash(1337, 2)

.text
.align 2

.private_extern _rootsqz_word_predict
_rootsqz_word_predict:
    b       _rootsqz_norder_byte_predict

.private_extern _rootsqz_word_learn
_rootsqz_word_learn:
    b       _rootsqz_norder_byte_learn

// ============================================================
// ln_mixer
// ============================================================

// AArch64 implementation of src/compressor/model.rs LnMixerPred.
//
// Context layout:
//     uint32_t num_models;
//     uint32_t bit_ctx;
//     uint32_t prev_byte;
//     uint32_t pad;
//     double last_total_p;
//     void **model_contexts;
//     double (**predict_fns)(void *);       // Rust Model::pred, stretched
//     void (**learn_fns)(void *, uint32_t);
//     double *base_weights;                 // num_models entries
//     double *ctx_weights;                  // 256 * 255 * num_models entries
//     uint8_t *ctx_initialized;             // 256 * 255 entries
//     double *last_p;                       // num_models entries

.text
.align 2

.equ LNM_NUM_MODELS,      0
.equ LNM_BIT_CTX,         4
.equ LNM_PREV_BYTE,       8
.equ LNM_LAST_TOTAL_P,   16
.equ LNM_MODEL_CONTEXTS, 24
.equ LNM_PREDICT_FNS,    32
.equ LNM_LEARN_FNS,      40
.equ LNM_BASE_WEIGHTS,   48
.equ LNM_CTX_WEIGHTS,    56
.equ LNM_CTX_INIT,       64
.equ LNM_LAST_P,         72
.equ LNM_SIZE,           80

.private_extern _rootsqz_ln_mixer_predict_stretched
_rootsqz_ln_mixer_predict_stretched:
    stp     x29, x30, [sp, #-112]!
    mov     x29, sp
    stp     x19, x20, [sp, #16]
    stp     x21, x22, [sp, #32]
    stp     x23, x24, [sp, #48]
    stp     x25, x26, [sp, #64]
    stp     x27, x28, [sp, #80]

    mov     x19, x0
    ldr     w20, [x19, #LNM_NUM_MODELS]
    ldr     x21, [x19, #LNM_MODEL_CONTEXTS]
    ldr     x22, [x19, #LNM_PREDICT_FNS]
    ldr     x23, [x19, #LNM_BASE_WEIGHTS]
    ldr     x24, [x19, #LNM_CTX_WEIGHTS]
    ldr     x25, [x19, #LNM_CTX_INIT]
    ldr     x26, [x19, #LNM_LAST_P]

    ldr     w9, [x19, #LNM_PREV_BYTE]
    ldr     w10, [x19, #LNM_BIT_CTX]
    sub     w10, w10, #1
    mov     w11, #255
    madd    w9, w9, w11, w10       // row = prev_byte * 255 + bit_ctx - 1
    ldrb    w28, [x25, x9]
    umull   x10, w9, w20
    add     x24, x24, x10, lsl #3  // current ctx weight row

    fmov    d0, xzr
    str     d0, [sp, #96]          // sum
    mov     x27, #0

1:
    cmp     x27, x20
    b.hs    3f

    ldr     x0, [x21, x27, lsl #3]
    ldr     x9, [x22, x27, lsl #3]
    blr     x9

    str     d0, [x26, x27, lsl #3]
    ldr     d1, [x23, x27, lsl #3]
    cbz     w28, 2f
    ldr     d2, [x24, x27, lsl #3]
    adrp    x9, L_rootsqz_ln_mixer_ctx_weight_scale@PAGE
    add     x9, x9, L_rootsqz_ln_mixer_ctx_weight_scale@PAGEOFF
    ldr     d3, [x9]
    fmadd   d1, d2, d3, d1
2:
    fmul    d0, d0, d1
    ldr     d4, [sp, #96]
    fadd    d4, d4, d0
    str     d4, [sp, #96]

    add     x27, x27, #1
    b       1b

3:
    ldr     d0, [sp, #96]           // d0 = sum (stretched prediction)
    str     d0, [sp, #104]          // save for return
    bl      _rootsqz_prob_squash    // d0 = squash(sum)
    str     d0, [x19, #LNM_LAST_TOTAL_P]
    ldr     d0, [sp, #104]          // return stretched sum

    ldp     x27, x28, [sp, #80]
    ldp     x25, x26, [sp, #64]
    ldp     x23, x24, [sp, #48]
    ldp     x21, x22, [sp, #32]
    ldp     x19, x20, [sp, #16]
    ldp     x29, x30, [sp], #112
    ret

.private_extern _rootsqz_ln_mixer_learn
_rootsqz_ln_mixer_learn:
    stp     x29, x30, [sp, #-128]!
    mov     x29, sp
    stp     x19, x20, [sp, #16]
    stp     x21, x22, [sp, #32]
    stp     x23, x24, [sp, #48]
    stp     x25, x26, [sp, #64]
    stp     x27, x28, [sp, #80]

    mov     x19, x0
    and     w28, w1, #1
    ldr     w20, [x19, #LNM_NUM_MODELS]
    ldr     x21, [x19, #LNM_MODEL_CONTEXTS]
    ldr     x22, [x19, #LNM_LEARN_FNS]
    ldr     x23, [x19, #LNM_BASE_WEIGHTS]
    ldr     x24, [x19, #LNM_CTX_WEIGHTS]
    ldr     x25, [x19, #LNM_CTX_INIT]
    ldr     x26, [x19, #LNM_LAST_P]

    ldr     w9, [x19, #LNM_PREV_BYTE]
    ldr     w10, [x19, #LNM_BIT_CTX]
    sub     w10, w10, #1
    mov     w11, #255
    madd    w9, w9, w11, w10       // row
    mov     w10, w9
    umull   x11, w9, w20
    add     x24, x24, x11, lsl #3  // current ctx weight row

    ldrb    w11, [x25, x10]
    cbnz    w11, 2f
    mov     x27, #0
1:
    cmp     x27, x20
    b.hs    11f
    ldr     d0, [x23, x27, lsl #3]
    str     d0, [x24, x27, lsl #3]
    add     x27, x27, #1
    b       1b
11:
    mov     w11, #1
    strb    w11, [x25, x10]

2:
    ucvtf   d0, w28
    ldr     d1, [x19, #LNM_LAST_TOTAL_P]
    fsub    d0, d0, d1
    str     d0, [sp, #96]          // pred_err

    mov     x27, #0
3:
    cmp     x27, x20
    b.hs    4f

    ldr     x0, [x21, x27, lsl #3]
    mov     w1, w28
    ldr     x9, [x22, x27, lsl #3]
    blr     x9

    ldr     d0, [sp, #96]
    ldr     d1, [x26, x27, lsl #3]
    fmul    d0, d0, d1             // pred_err * last_p[i]

    adrp    x9, L_rootsqz_ln_mixer_learning_rate@PAGE
    add     x9, x9, L_rootsqz_ln_mixer_learning_rate@PAGEOFF
    ldr     d2, [x9]
    ldr     d3, [x23, x27, lsl #3]
    fmul    d4, d0, d2
    fadd    d3, d3, d4
    str     d3, [x23, x27, lsl #3]

    adrp    x9, L_rootsqz_ln_mixer_learning_rate_ctx@PAGE
    add     x9, x9, L_rootsqz_ln_mixer_learning_rate_ctx@PAGEOFF
    ldr     d2, [x9]
    ldr     d3, [x24, x27, lsl #3]
    fmul    d4, d0, d2
    fadd    d3, d3, d4
    str     d3, [x24, x27, lsl #3]

    add     x27, x27, #1
    b       3b

4:
    ldr     w9, [x19, #LNM_BIT_CTX]
    lsl     w9, w9, #1
    orr     w9, w9, w28
    cmp     w9, #256
    b.hs    5f
    str     w9, [x19, #LNM_BIT_CTX]
    b       6f
5:
    and     w9, w9, #0xff
    str     w9, [x19, #LNM_PREV_BYTE]
    mov     w9, #1
    str     w9, [x19, #LNM_BIT_CTX]

6:
    ldp     x27, x28, [sp, #80]
    ldp     x25, x26, [sp, #64]
    ldp     x23, x24, [sp, #48]
    ldp     x21, x22, [sp, #32]
    ldp     x19, x20, [sp, #16]
    ldp     x29, x30, [sp], #128
    ret

.section __TEXT,__literal8,8byte_literals
.p2align 3
L_rootsqz_ln_mixer_ctx_weight_scale:
    .double 0.3
L_rootsqz_ln_mixer_learning_rate:
    .double 0.0004
L_rootsqz_ln_mixer_learning_rate_ctx:
    .double 0.022

