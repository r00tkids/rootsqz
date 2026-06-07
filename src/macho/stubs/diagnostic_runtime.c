#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

struct rootsqzSegment {
    uint64_t offset;
    uint64_t size;
    uint32_t prot;
    uint32_t reserved;
};

struct rootsqzImport {
    uintptr_t address;
    uint32_t weak;
    uint32_t reserved;
};

struct rootsqzFixup {
    uint64_t offset;
    uint64_t target;
    uint64_t addend;
    uint32_t import_index;
    uint32_t high8;
    uint32_t kind;
    uint32_t reserved;
};

extern const uint64_t rootsqz_image_size;
extern const uint64_t rootsqz_entry_offset;
extern const struct rootsqzSegment rootsqz_segments_start[];
extern const struct rootsqzSegment rootsqz_segments_end[];
extern const struct rootsqzImport rootsqz_imports_start[];
extern const struct rootsqzImport rootsqz_imports_end[];
extern const struct rootsqzFixup rootsqz_fixups_start[];
extern const struct rootsqzFixup rootsqz_fixups_end[];

static uint64_t page_floor(uint64_t value, uint64_t page_size) {
    return value & ~(page_size - 1);
}

static uint64_t page_ceil(uint64_t value, uint64_t page_size) {
    return (value + page_size - 1) & ~(page_size - 1);
}

void *rootsqz_prepare_image(void) {
    void *image = mmap(NULL, (size_t)rootsqz_image_size, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANON, -1, 0);
    if (image == MAP_FAILED) {
        fprintf(stderr, "rootsqz: mmap failed: %s\n", strerror(errno));
        exit(1);
    }
    return image;
}

static uintptr_t resolve_import(uint32_t index) {
    size_t count = (size_t)(rootsqz_imports_end - rootsqz_imports_start);
    if (index >= count) {
        fprintf(stderr, "rootsqz: invalid import index %u\n", index);
        exit(1);
    }

    const struct rootsqzImport *import = &rootsqz_imports_start[index];
    if (import->address == 0 && !import->weak) {
        fprintf(stderr, "rootsqz: import %u resolved to null\n", index);
        exit(1);
    }
    return import->address;
}

static void apply_fixups(uint8_t *image) {
    for (const struct rootsqzFixup *fixup = rootsqz_fixups_start;
         fixup < rootsqz_fixups_end;
         fixup++) {
        uintptr_t *slot = (uintptr_t *)(image + fixup->offset);
        if (fixup->kind == 1) {
            *slot = resolve_import(fixup->import_index) + (uintptr_t)fixup->addend;
        } else {
            uintptr_t pointer = (uintptr_t)image + (uintptr_t)fixup->target;
            pointer |= (uintptr_t)fixup->high8 << 56;
            *slot = pointer;
        }
    }
}

static void protect_segments(uint8_t *image) {
    uint64_t page_size = (uint64_t)getpagesize();
    for (const struct rootsqzSegment *segment = rootsqz_segments_start;
         segment < rootsqz_segments_end;
         segment++) {
        if (segment->size == 0) {
            continue;
        }

        uint64_t start = page_floor(segment->offset, page_size);
        uint64_t end = page_ceil(segment->offset + segment->size, page_size);
        if (mprotect(image + start, (size_t)(end - start), (int)segment->prot) != 0) {
            fprintf(stderr, "rootsqz: mprotect failed: %s\n", strerror(errno));
            exit(1);
        }
    }
}

int rootsqz_launch_image(uint8_t *image, int argc, char **argv, char **envp) {
    printf("Testing!\n");
    apply_fixups(image);
    protect_segments(image);
    __builtin___clear_cache((char *)image, (char *)image + rootsqz_image_size);

    int (*entry)(int, char **, char **) =
        (int (*)(int, char **, char **))(void *)(image + rootsqz_entry_offset);
    return entry(argc, argv, envp);
}
