/* Host-only gated SDK. Bump-only mspaces deliberately force cross-arena realloc;
 * production policy, synchronization, lookup and rollback remain in native.c. */
#include "mock_sdk.h"
#include <stdlib.h>
#include <string.h>
#define MOCK_BLOCKS 2048
#define MOCK_CAPACITY (32u * 1024u * 1024u)
typedef struct { void *base; uint32_t bytes; } Block;
typedef struct { uint32_t capacity, cursor, live; } Space;
typedef struct { uint32_t size, live; } Allocation;
static Block blocks[MOCK_BLOCKS];
static uint32_t used;
static int fail_alloc, fail_base, fail_free, fail_space;
void vn_mock_fail(int allocation, int address, int release, int mspace) {
    fail_alloc = allocation; fail_base = address; fail_free = release; fail_space = mspace;
}
uint32_t vn_mock_committed(void) { return used; }
int sceKernelCreateLwMutex(SceKernelLwMutexWork *m, const char *n, int a, int c, void *o) {
    (void)n; (void)a; (void)c; (void)o;
#ifdef _WIN32
    InitializeCriticalSection(m); return 0;
#else
    pthread_mutexattr_t attr;
    pthread_mutexattr_init(&attr);
    pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    int result = pthread_mutex_init(m, &attr);
    pthread_mutexattr_destroy(&attr);
    return result;
#endif
}
int sceKernelLockLwMutex(SceKernelLwMutexWork *m, int n, void *t) {
    (void)n; (void)t;
#ifdef _WIN32
    EnterCriticalSection(m); return 0;
#else
    return pthread_mutex_lock(m);
#endif
}
int sceKernelUnlockLwMutex(SceKernelLwMutexWork *m, int n) {
    (void)n;
#ifdef _WIN32
    LeaveCriticalSection(m); return 0;
#else
    return pthread_mutex_unlock(m);
#endif
}
int sceKernelDeleteLwMutex(SceKernelLwMutexWork *m) {
#ifdef _WIN32
    DeleteCriticalSection(m); return 0;
#else
    return pthread_mutex_destroy(m);
#endif
}
int sceKernelAllocMemBlock(const char *name, int type, uint32_t bytes, void *opt) {
    (void)name; (void)type; (void)opt;
    if (fail_alloc || bytes > MOCK_CAPACITY - used) return -41;
    for (int i = 0; i < MOCK_BLOCKS; ++i) if (!blocks[i].base) {
        void *p = malloc(bytes);
        if (!p) return -41;
        blocks[i] = (Block){p, bytes}; used += bytes; return i + 1;
    }
    return -42;
}
int sceKernelGetMemBlockBase(int uid, void **base) {
    if (fail_base) return -43;
    *base = blocks[uid - 1].base; return 0;
}
int sceKernelFreeMemBlock(int uid) {
    if (fail_free) return -44;
    Block *b = &blocks[uid - 1];
    if (!b->base) return -45;
    free(b->base); used -= b->bytes; *b = (Block){0}; return 0;
}
int sceKernelGetFreeMemorySize(SceKernelFreeMemorySizeInfo *out) {
    out->size_user = MOCK_CAPACITY - used; return 0;
}
SceClibMspace sceClibMspaceCreate(void *base, uint32_t bytes) {
    if (fail_space) return NULL;
    *(Space *)base = (Space){bytes, sizeof(Space), 0}; return base;
}
void sceClibMspaceDestroy(SceClibMspace s) { (void)s; }
void sceClibMspaceMallocStatsFast(SceClibMspace s, SceClibMspaceStats *out) {
    Space *space = s; *out = (SceClibMspaceStats){space->capacity, space->capacity, space->cursor, space->cursor};
}
void *sceClibMspaceMemalign(SceClibMspace m, uint32_t alignment, uint32_t bytes) {
    Space *s = m;
    uintptr_t base = (uintptr_t)m;
    uintptr_t p = (base + s->cursor + sizeof(Allocation) + alignment - 1) & ~(uintptr_t)(alignment - 1);
    if (p - base > s->capacity || bytes > s->capacity - (p - base)) return NULL;
    Allocation *a = (Allocation *)(p - sizeof(Allocation));
    *a = (Allocation){bytes, 1};
    s->cursor = p - base + bytes; ++s->live; return (void *)p;
}
void *sceClibMspaceMalloc(SceClibMspace m, uint32_t bytes) { return sceClibMspaceMemalign(m, 8, bytes); }
void *sceClibMspaceRealloc(SceClibMspace m, void *p, uint32_t size) {
    (void)m;
    Allocation *a = (Allocation *)((char *)p - sizeof(Allocation));
    return size <= a->size ? p : NULL;
}
void sceClibMspaceFree(SceClibMspace m, void *p) {
    Allocation *a = (Allocation *)((char *)p - sizeof(Allocation));
    a->live = 0; --((Space *)m)->live;
}
int sceClibMspaceIsHeapEmpty(SceClibMspace m) { return ((Space *)m)->live == 0; }
uint32_t sceClibMspaceMallocUsableSize(void *p) {
    return ((Allocation *)((char *)p - sizeof(Allocation)))->size;
}
void *sceClibMemset(void *p, int c, uint32_t n) { return memset(p, c, n); }
void *sceClibMemcpy(void *p, const void *q, uint32_t n) { return memcpy(p, q, n); }
