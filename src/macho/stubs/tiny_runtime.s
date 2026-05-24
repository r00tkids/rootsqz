// AArch64 Mach-O tiny runtime — hand-written replacement for tiny_runtime.c.

.text
.align 2

.equ SYS_MMAP,      197
.equ SYS_MPROTECT,  74

// rootsqzFixup field offsets
.equ FX_OFFSET,     0
.equ FX_TARGET,     8
.equ FX_ADDEND,     16
.equ FX_IMPORT_IDX, 24
.equ FX_HIGH8,      28
.equ FX_KIND,       32
.equ FIXUP_STRIDE,  40

// rootsqzSegment field offsets
.equ SEG_OFFSET,    0
.equ SEG_SIZE,      8
.equ SEG_PROT,      16
.equ SEG_STRIDE,    24

// rootsqzImport field offsets
.equ IMP_ADDRESS,   0
.equ IMPORT_SHIFT,  4           // log2(sizeof(rootsqzImport)=16)

// ---------------------------------------------------------------------------
// _rootsqz_prepare_image — leaf, no frame
// mmap(NULL, image_size, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANON, -1, 0)
// Returns void* in x0.  On failure x0 holds Darwin's errno (not MAP_FAILED);
// bootstrap.s stores it unconditionally so either value crashes the same way.
// ---------------------------------------------------------------------------
.globl _rootsqz_prepare_image
_rootsqz_prepare_image:
    adrp    x1, _rootsqz_image_size@PAGE
    add     x1, x1, _rootsqz_image_size@PAGEOFF
    ldr     x1, [x1]                // length = image_size
    mov     x0, xzr                 // addr = NULL
    mov     w2, #3                  // PROT_READ | PROT_WRITE
    movz    w3, #0x1002             // MAP_PRIVATE | MAP_ANON
    mov     x4, #-1                 // fd = -1
    mov     x5, xzr                 // offset = 0
    mov     x16, #SYS_MMAP
    svc     #0x80
    ret


// ---------------------------------------------------------------------------
// _rootsqz_launch_image
// x0=image, w1=argc, x2=argv, x3=envp -> int (tail-called entry return value)
//
// Frame (16 bytes):
//   sp+0:  x29, x30
// ---------------------------------------------------------------------------
.globl _rootsqz_launch_image
_rootsqz_launch_image:
    stp     x29, x30, [sp, #-16]!

    mov     x19, x0
    mov     w20, w1
    mov     x21, x2
    mov     x22, x3

    // --- apply_fixups ---
    // Scratch: x9=fixup_ptr, x10=fixups_end, x11=imports_start, x12-x15=temps
    // No bl inside this loop; svc not used; all scratch regs survive.
    adrp    x9,  _rootsqz_fixups_start@PAGE
    add     x9,  x9,  _rootsqz_fixups_start@PAGEOFF
    adrp    x10, _rootsqz_fixups_end@PAGE
    add     x10, x10, _rootsqz_fixups_end@PAGEOFF
    adrp    x11, _rootsqz_imports_start@PAGE
    add     x11, x11, _rootsqz_imports_start@PAGEOFF

1:
    cmp     x9, x10
    b.hs    2f

    ldr     x13, [x9, #FX_OFFSET]
    add     x13, x19, x13          // slot = image + fixup->offset
    ldr     w12, [x9, #FX_KIND]
    cmp     w12, #1
    b.ne    3f

    // Import fixup: *slot = imports[import_index].address + addend
    ldr     w12, [x9, #FX_IMPORT_IDX]
    ldr     x14, [x9, #FX_ADDEND]
    add     x15, x11, x12, lsl #IMPORT_SHIFT
    ldr     x15, [x15, #IMP_ADDRESS]
    add     x15, x15, x14
    str     x15, [x13]
    b       4f

3:  // Pointer fixup: *slot = (image + target) | (high8 << 56)
    ldr     x14, [x9, #FX_TARGET]
    ldr     w15, [x9, #FX_HIGH8]
    add     x14, x19, x14
    lsl     x15, x15, #56
    orr     x14, x14, x15
    str     x14, [x13]

4:
    add     x9, x9, #FIXUP_STRIDE
    b       1b

2:  // end apply_fixups

    // --- clear_instruction_cache(image, image + image_size) ---
    // Scratch: x9=cache_line_ptr, x10=end
    adrp    x10, _rootsqz_image_size@PAGE
    add     x10, x10, _rootsqz_image_size@PAGEOFF
    ldr     x10, [x10]
    add     x10, x19, x10          // end = image + image_size
    bic     x9, x19, #63           // p = image & ~63

    dsb     ish
5:
    cmp     x9, x10
    b.hs    6f
    ic      ivau, x9
    add     x9, x9, #64
    b       5b
6:
    dsb     ish
    isb

    // --- protect_segments ---
    // Scratch: x9=seg_ptr, x10=segs_end, x11-x13=temps, x16=syscall number
    // Darwin svc only clobbers x0 and carry; x9, x10, x16 survive each svc.
    adrp    x9,  _rootsqz_segments_start@PAGE
    add     x9,  x9,  _rootsqz_segments_start@PAGEOFF
    adrp    x10, _rootsqz_segments_end@PAGE
    add     x10, x10, _rootsqz_segments_end@PAGEOFF
    mov     x16, #SYS_MPROTECT

7:
    cmp     x9, x10
    b.hs    8f

    ldr     x11, [x9, #SEG_SIZE]
    cbz     x11, 9f

    ldr     x12, [x9, #SEG_OFFSET]
    bic     x13, x12, #0x3FFF      // start = page_floor(offset)
    add     x12, x12, x11          // offset + size
    add     x12, x12, #0x3000      // bias: high 12 bits of 0x3FFF
    add     x12, x12, #0x0FFF      // bias: low 12 bits of 0x3FFF
    bic     x12, x12, #0x3FFF      // end = page_ceil(offset + size)

    add     x0, x19, x13           // addr = image + start
    sub     x1, x12, x13           // length = end - start
    ldr     w2, [x9, #SEG_PROT]
    svc     #0x80                   // mprotect; return value ignored

9:
    add     x9, x9, #SEG_STRIDE
    b       7b

8:  // end protect_segments

    // --- tail call to entry point ---
    // Load entry address into x9 before frame teardown (x19 still live).
    adrp    x9, _rootsqz_entry_offset@PAGE
    add     x9, x9, _rootsqz_entry_offset@PAGEOFF
    ldr     x9, [x9]
    add     x9, x19, x9            // entry = image + entry_offset

    // Remap args BEFORE restoring callee-saved regs — after the ldp restores,
    // x20/x21/x22 hold bootstrap.s's saved values, not our argc/argv/envp.
    // x0/x1/x2 are not written by any of the ldp instructions below.
    mov     w0, w20                 // argc
    mov     x1, x21                 // argv
    mov     x2, x22                 // envp

    ldp     x29, x30, [sp], #16
    br      x9                      // tail call; entry's ret goes to bootstrap.s
