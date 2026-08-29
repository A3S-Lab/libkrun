/*
 * Extract the already-prepared kernel bundle exported by libkrunfw.
 *
 * The returned bytes are not an ELF vmlinux. They are a flattened guest-memory
 * image, so the load and entry addresses exported by the same library must stay
 * attached to it as metadata.
 *
 * Build:
 *   cc -std=c11 -Wall -Wextra -Werror -o extract_kernel extract_kernel.c -ldl
 *
 * Usage:
 *   ./extract_kernel <libkrunfw.so> <raw-output> <metadata-output>
 */

#include <dlfcn.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef char *(*krunfw_get_kernel_fn)(size_t *, size_t *, size_t *);

static int close_checked(FILE *file, const char *description) {
    if (fclose(file) != 0) {
        fprintf(stderr, "failed to close %s\n", description);
        return -1;
    }
    return 0;
}

int main(int argc, char *argv[]) {
    if (argc != 4) {
        fprintf(stderr,
                "Usage: %s <path-to-libkrunfw.so> <raw-output> "
                "<metadata-output>\n",
                argv[0]);
        return 2;
    }
    if (sizeof(size_t) != sizeof(uint64_t)) {
        fprintf(stderr, "extract_kernel requires a 64-bit host\n");
        return 1;
    }

    const char *lib_path = argv[1];
    const char *raw_path = argv[2];
    const char *metadata_path = argv[3];
    void *library = dlopen(lib_path, RTLD_NOW | RTLD_LOCAL);
    if (library == NULL) {
        fprintf(stderr, "dlopen(%s) failed: %s\n", lib_path, dlerror());
        return 1;
    }

    dlerror();
    void *symbol = dlsym(library, "krunfw_get_kernel");
    const char *symbol_error = dlerror();
    if (symbol_error != NULL || symbol == NULL) {
        fprintf(stderr, "dlsym(krunfw_get_kernel) failed: %s\n",
                symbol_error != NULL ? symbol_error : "symbol is null");
        dlclose(library);
        return 1;
    }
    if (sizeof(symbol) != sizeof(krunfw_get_kernel_fn)) {
        fprintf(stderr, "object and function pointers have incompatible sizes\n");
        dlclose(library);
        return 1;
    }

    krunfw_get_kernel_fn get_kernel = NULL;
    memcpy(&get_kernel, &symbol, sizeof(get_kernel));

    size_t guest_load_addr = 0;
    size_t entry_addr = 0;
    size_t bundle_size = 0;
    char *bundle = get_kernel(&guest_load_addr, &entry_addr, &bundle_size);
    if (bundle == NULL || bundle_size == 0) {
        fprintf(stderr, "krunfw_get_kernel returned a null or empty bundle\n");
        dlclose(library);
        return 1;
    }
    if (guest_load_addr == 0 || (guest_load_addr % 4096) != 0 ||
        entry_addr == 0 || (bundle_size % 4096) != 0) {
        fprintf(stderr,
                "krunfw_get_kernel returned invalid alignment/address metadata: "
                "load=0x%" PRIx64 " entry=0x%" PRIx64 " size=%zu\n",
                (uint64_t)guest_load_addr, (uint64_t)entry_addr, bundle_size);
        dlclose(library);
        return 1;
    }
    if ((uint64_t)guest_load_addr > UINT64_MAX - (uint64_t)bundle_size) {
        fprintf(stderr, "kernel bundle guest address range overflows u64\n");
        dlclose(library);
        return 1;
    }

    FILE *raw_file = fopen(raw_path, "wb");
    if (raw_file == NULL) {
        perror("cannot open raw kernel bundle output");
        dlclose(library);
        return 1;
    }
    if (fwrite(bundle, 1, bundle_size, raw_file) != bundle_size) {
        perror("cannot write raw kernel bundle");
        fclose(raw_file);
        dlclose(library);
        return 1;
    }
    if (close_checked(raw_file, "raw kernel bundle") != 0) {
        dlclose(library);
        return 1;
    }

    FILE *metadata_file = fopen(metadata_path, "wb");
    if (metadata_file == NULL) {
        perror("cannot open raw bundle metadata output");
        dlclose(library);
        return 1;
    }
    int written = fprintf(
        metadata_file,
        "format=libkrunfw-raw-bundle-v1\n"
        "generator=scripts/extract_kernel.c\n"
        "guest_load_addr=0x%016" PRIx64 "\n"
        "entry_addr=0x%016" PRIx64 "\n"
        "bundle_size=%zu\n",
        (uint64_t)guest_load_addr, (uint64_t)entry_addr, bundle_size);
    if (written < 0) {
        perror("cannot write raw bundle metadata");
        fclose(metadata_file);
        dlclose(library);
        return 1;
    }
    if (close_checked(metadata_file, "raw bundle metadata") != 0) {
        dlclose(library);
        return 1;
    }

    printf("extracted raw kernel bundle: load=0x%" PRIx64
           " entry=0x%" PRIx64 " size=%zu bytes\n",
           (uint64_t)guest_load_addr, (uint64_t)entry_addr, bundle_size);
    dlclose(library);
    return 0;
}
