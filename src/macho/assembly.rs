use crate::compressor::model::{MODEL4K_NORDER_MASKS, MODEL4K_NUM_MODELS};

pub const NORDER_RECORD_BYTES: usize = 4;
const MIXER_CONTEXT_ROWS: usize = 256 * 255;
const LN_MIXER_CONTEXT_BYTES: usize = 80;
const NORDER_CONTEXT_BYTES: usize = 56;

pub fn render_model4k_assembly(table_pow2: u32) -> String {
    let table_len = 1usize << table_pow2;
    let table_bytes = table_len * NORDER_RECORD_BYTES;
    let hash_mask = table_len - 1;
    let child_context_bytes = MODEL4K_NUM_MODELS * NORDER_CONTEXT_BYTES;
    let pointer_array_bytes = MODEL4K_NUM_MODELS * size_of::<u64>();
    let base_weight_bytes = MODEL4K_NUM_MODELS * size_of::<f64>();
    let ctx_weight_bytes = MIXER_CONTEXT_ROWS * MODEL4K_NUM_MODELS * size_of::<f64>();
    let initial_weight = 1.0 / MODEL4K_NUM_MODELS as f64;
    let masks = MODEL4K_NORDER_MASKS
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#".section __TEXT,__const
_rootsqz_model4k_masks:
    .byte {masks}
.p2align 3
_rootsqz_model4k_hash_mask:
    .quad {hash_mask}

.section __TEXT,__literal8,8byte_literals
.p2align 3
_rootsqz_model4k_initial_weight:
    .double {initial_weight:.17}

.section __DATA,__bss
.p2align 3
.globl _rootsqz_model_ctx
_rootsqz_model_ctx:
    .space {mixer_context_bytes}
.p2align 3
.globl _rootsqz_model4k_initialized
_rootsqz_model4k_initialized:
    .space 1
.p2align 3
.globl _rootsqz_model4k_children
_rootsqz_model4k_children:
    .space {child_context_bytes}
.p2align 3
.globl _rootsqz_model4k_child_contexts
_rootsqz_model4k_child_contexts:
    .space {pointer_array_bytes}
.p2align 3
.globl _rootsqz_model4k_predict_fns
_rootsqz_model4k_predict_fns:
    .space {pointer_array_bytes}
.p2align 3
.globl _rootsqz_model4k_learn_fns
_rootsqz_model4k_learn_fns:
    .space {pointer_array_bytes}
.p2align 3
.globl _rootsqz_model4k_base_weights
_rootsqz_model4k_base_weights:
    .space {base_weight_bytes}
.p2align 3
.globl _rootsqz_model4k_last_p
_rootsqz_model4k_last_p:
    .space {base_weight_bytes}
.p2align 3
.globl _rootsqz_model4k_ctx_init
_rootsqz_model4k_ctx_init:
    .space {ctx_init_bytes}
.p2align 3
.globl _rootsqz_model4k_ctx_weights
_rootsqz_model4k_ctx_weights:
    .space {ctx_weight_bytes}

.zerofill __DATA,__bss,_rootsqz_norder_table,{table_bytes},2

.text
.align 2
_rootsqz_model4k_init:
    adrp    x9, _rootsqz_model4k_initialized@PAGE
    add     x9, x9, _rootsqz_model4k_initialized@PAGEOFF
    ldrb    w10, [x9]
    cbnz    w10, 9f

    adrp    x8, _rootsqz_model_ctx@PAGE
    add     x8, x8, _rootsqz_model_ctx@PAGEOFF
    adrp    x9, _rootsqz_model4k_children@PAGE
    add     x9, x9, _rootsqz_model4k_children@PAGEOFF
    adrp    x10, _rootsqz_model4k_child_contexts@PAGE
    add     x10, x10, _rootsqz_model4k_child_contexts@PAGEOFF
    adrp    x11, _rootsqz_model4k_predict_fns@PAGE
    add     x11, x11, _rootsqz_model4k_predict_fns@PAGEOFF
    adrp    x12, _rootsqz_model4k_learn_fns@PAGE
    add     x12, x12, _rootsqz_model4k_learn_fns@PAGEOFF
    adrp    x13, _rootsqz_model4k_base_weights@PAGE
    add     x13, x13, _rootsqz_model4k_base_weights@PAGEOFF
    adrp    x14, _rootsqz_model4k_masks@PAGE
    add     x14, x14, _rootsqz_model4k_masks@PAGEOFF
    adrp    x15, _rootsqz_norder_table@PAGE
    add     x15, x15, _rootsqz_norder_table@PAGEOFF
    adrp    x6, _rootsqz_model4k_hash_mask@PAGE
    add     x6, x6, _rootsqz_model4k_hash_mask@PAGEOFF
    ldr     x16, [x6]
    adrp    x17, _rootsqz_model4k_initial_weight@PAGE
    add     x17, x17, _rootsqz_model4k_initial_weight@PAGEOFF

    mov     w6, #{num_models}
    str     w6, [x8, #0]
    mov     w6, #1
    str     w6, [x8, #4]
    str     wzr, [x8, #8]
    str     wzr, [x8, #12]
    str     xzr, [x8, #16]
    str     x10, [x8, #24]
    str     x11, [x8, #32]
    str     x12, [x8, #40]
    str     x13, [x8, #48]
    adrp    x6, _rootsqz_model4k_ctx_weights@PAGE
    add     x6, x6, _rootsqz_model4k_ctx_weights@PAGEOFF
    str     x6, [x8, #56]
    adrp    x6, _rootsqz_model4k_ctx_init@PAGE
    add     x6, x6, _rootsqz_model4k_ctx_init@PAGEOFF
    str     x6, [x8, #64]
    adrp    x6, _rootsqz_model4k_last_p@PAGE
    add     x6, x6, _rootsqz_model4k_last_p@PAGEOFF
    str     x6, [x8, #72]

    mov     x0, #0
1:
    cmp     x0, #{norder_count}
    b.hs    5f

    mov     x2, #{norder_context_bytes}
    madd    x1, x0, x2, x9
    str     x1, [x10, x0, lsl #3]
    adrp    x6, _rootsqz_norder_byte_predict@PAGE
    add     x6, x6, _rootsqz_norder_byte_predict@PAGEOFF
    str     x6, [x11, x0, lsl #3]
    adrp    x6, _rootsqz_norder_byte_learn@PAGE
    add     x6, x6, _rootsqz_norder_byte_learn@PAGEOFF
    str     x6, [x12, x0, lsl #3]
    ldr     d0, [x17]
    str     d0, [x13, x0, lsl #3]

    ldrb    w2, [x14, x0]
    str     wzr, [x1, #0]
    mov     w3, #1
    str     w3, [x1, #4]
    lsr     w3, w2, #2
    eor     w3, w2, w3
    movz    w4, #0xa7bd
    movk    w4, #0x9e35, lsl #16
    mul     w3, w3, w4
    lsr     w3, w3, #2
    str     w3, [x1, #8]
    mov     w3, #15
    str     w3, [x1, #12]
    str     xzr, [x1, #16]

    mov     x3, #0
    mov     x4, #0
2:
    cmp     x4, #8
    b.hs    4f
    lsr     w5, w2, w4
    tbz     w5, #0, 3f
    mov     x6, #0xff
    lsl     x5, x4, #3
    lsl     x6, x6, x5
    orr     x3, x3, x6
3:
    add     x4, x4, #1
    b       2b
4:
    str     x3, [x1, #24]
    str     wzr, [x1, #32]
    str     wzr, [x1, #36]
    str     x15, [x1, #40]
    str     x16, [x1, #48]
    add     x0, x0, #1
    b       1b

5:
    mov     x2, #{norder_context_bytes}
    madd    x1, x0, x2, x9
    str     x1, [x10, x0, lsl #3]
    adrp    x6, _rootsqz_word_predict@PAGE
    add     x6, x6, _rootsqz_word_predict@PAGEOFF
    str     x6, [x11, x0, lsl #3]
    adrp    x6, _rootsqz_word_learn@PAGE
    add     x6, x6, _rootsqz_word_learn@PAGEOFF
    str     x6, [x12, x0, lsl #3]
    ldr     d0, [x17]
    str     d0, [x13, x0, lsl #3]

    str     wzr, [x1, #0]
    mov     w2, #1
    str     w2, [x1, #4]
    mov     w2, #1337
    lsr     w3, w2, #2
    eor     w3, w2, w3
    movz    w4, #0xa7bd
    movk    w4, #0x9e35, lsl #16
    mul     w3, w3, w4
    lsr     w3, w3, #2
    str     w3, [x1, #8]
    mov     w2, #15
    str     w2, [x1, #12]
    movz    w2, #0x9dc5
    movk    w2, #0x811c, lsl #16
    str     x2, [x1, #16]
    mov     x2, #-1
    str     x2, [x1, #24]
    mov     w2, #1
    str     w2, [x1, #32]
    str     wzr, [x1, #36]
    str     x15, [x1, #40]
    str     x16, [x1, #48]

    adrp    x9, _rootsqz_model4k_initialized@PAGE
    add     x9, x9, _rootsqz_model4k_initialized@PAGEOFF
    mov     w10, #1
    strb    w10, [x9]
9:
    ret

.globl _rootsqz_model_predict
_rootsqz_model_predict:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp
    bl      _rootsqz_model4k_init
    adrp    x0, _rootsqz_model_ctx@PAGE
    add     x0, x0, _rootsqz_model_ctx@PAGEOFF
    bl      _rootsqz_ln_mixer_predict_stretched
    bl      _rootsqz_prob_squash
    ldp     x29, x30, [sp], #16
    ret

.globl _rootsqz_model_learn
_rootsqz_model_learn:
    stp     x29, x30, [sp, #-32]!
    mov     x29, sp
    str     x19, [sp, #16]
    mov     w19, w1
    bl      _rootsqz_model4k_init
    adrp    x0, _rootsqz_model_ctx@PAGE
    add     x0, x0, _rootsqz_model_ctx@PAGEOFF
    mov     w1, w19
    bl      _rootsqz_ln_mixer_learn
    ldr     x19, [sp, #16]
    ldp     x29, x30, [sp], #32
    ret
"#,
        mixer_context_bytes = LN_MIXER_CONTEXT_BYTES,
        num_models = MODEL4K_NUM_MODELS,
        norder_count = MODEL4K_NORDER_MASKS.len(),
        norder_context_bytes = NORDER_CONTEXT_BYTES,
        ctx_init_bytes = MIXER_CONTEXT_ROWS,
    )
}
