/*
 * verify_dll.c
 *
 * Quick smoke-test: load libkrunfw.dll and call krunfw_get_kernel() to verify
 * the DLL exports the right symbols and the returned pointer is 4096-aligned.
 *
 * Build on Windows (MSVC):
 *   cl verify_dll.c
 *
 * Build on Linux (for testing a .so):
 *   gcc -o verify_dll verify_dll.c -ldl
 *
 * Usage:
 *   verify_dll.exe                       (looks for libkrunfw.dll in PATH)
 *   verify_dll.exe path\to\libkrunfw.dll
 */

#ifdef _WIN32
#  include <windows.h>
#  define LOAD_LIB(p)    LoadLibraryA(p)
#  define GET_SYM(h, s)  GetProcAddress((HMODULE)(h), s)
#  define CLOSE_LIB(h)   FreeLibrary((HMODULE)(h))
#  define LIB_ERR()      GetLastError()
typedef HMODULE lib_handle_t;
#else
#  include <dlfcn.h>
#  define LOAD_LIB(p)    dlopen(p, RTLD_LAZY)
#  define GET_SYM(h, s)  dlsym(h, s)
#  define CLOSE_LIB(h)   dlclose(h)
#  define LIB_ERR()      dlerror()
typedef void *lib_handle_t;
#endif

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef char *(*krunfw_get_kernel_fn)(uint64_t *, uint64_t *, size_t *);

int main(int argc, char *argv[]) {
    const char *lib_path = argc > 1 ? argv[1] : "libkrunfw.dll";

    printf("Loading: %s\n", lib_path);
    lib_handle_t lib = LOAD_LIB(lib_path);
    if (!lib) {
#ifdef _WIN32
        fprintf(stderr, "LoadLibrary failed: error %lu\n", LIB_ERR());
#else
        fprintf(stderr, "dlopen failed: %s\n", LIB_ERR());
#endif
        return 1;
    }

    krunfw_get_kernel_fn get_kernel =
        (krunfw_get_kernel_fn)GET_SYM(lib, "krunfw_get_kernel");
    if (!get_kernel) {
        fprintf(stderr, "krunfw_get_kernel not found in DLL\n");
        CLOSE_LIB(lib);
        return 1;
    }
    printf("krunfw_get_kernel found at %p\n", (void *)get_kernel);

    uint64_t guest_addr = 0, entry_addr = 0;
    size_t size = 0;
    char *ptr = get_kernel(&guest_addr, &entry_addr, &size);

    printf("  guest_addr  = 0x%llx\n", (unsigned long long)guest_addr);
    printf("  entry_addr  = 0x%llx\n", (unsigned long long)entry_addr);
    printf("  size        = %zu bytes (%.1f MB)\n", size, (double)size / 1048576.0);
    printf("  host_ptr    = %p\n", (void *)ptr);

    /* libkrun requires host_addr to be 4096-byte aligned */
    uintptr_t addr = (uintptr_t)ptr;
    if (addr == 0) {
        fprintf(stderr, "FAIL: returned null pointer\n");
        CLOSE_LIB(lib);
        return 1;
    }
    if (addr & 0xFFF) {
        fprintf(stderr, "FAIL: pointer is NOT 4096-byte aligned (addr & 0xFFF = 0x%lx)\n",
                (unsigned long)(addr & 0xFFF));
        CLOSE_LIB(lib);
        return 1;
    }
    printf("  alignment   = OK (4096-byte aligned)\n");

    /* Spot-check: guest_addr must also be page-aligned */
    if (guest_addr & 0xFFF) {
        fprintf(stderr, "FAIL: guest_addr is not page-aligned\n");
        CLOSE_LIB(lib);
        return 1;
    }

    /* Read first 4 bytes — should be ELF magic \x7fELF */
    unsigned char *bytes = (unsigned char *)ptr;
    printf("  first bytes = %02x %02x %02x %02x", bytes[0], bytes[1], bytes[2], bytes[3]);
    if (bytes[0] == 0x7f && bytes[1] == 'E' && bytes[2] == 'L' && bytes[3] == 'F') {
        printf("  (ELF magic OK)\n");
    } else {
        printf("  (WARNING: not ELF magic)\n");
    }

    CLOSE_LIB(lib);
    printf("\nPASS\n");
    return 0;
}
