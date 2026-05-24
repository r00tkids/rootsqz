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

.globl _rootsqz_norder_byte_predict
_rootsqz_norder_byte_predict:
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
    and     w0, w0, w9
    b       _rootsqz_prob_stretch_u24

.globl _rootsqz_norder_byte_learn
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
