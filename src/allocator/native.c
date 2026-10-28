/* Process-wide C/newlib/Rust allocation. No callback into Rust or malloc here.
 * Each arena owns one kernel UID and one sceClibMspace. The outer kernel mutex
 * protects the fixed descriptor/index arrays and every mspace operation. */
#include <stdint.h>
#include <stddef.h>
#include <limits.h>
#include <stdatomic.h>
#ifdef VITA_HEAP_TEST
#include "mock_sdk.h"
#else
#include <psp2/kernel/clib.h>
#include <psp2/kernel/sysmem.h>
#include <psp2/kernel/threadmgr.h>
#endif

#define ARENA_LIMIT 2048
#define QUANTUM (1024u * 1024u)
#define PAGE 4096u
#define READY 1
#define RETIRED 2

typedef struct Arena {
    void *base;
    SceClibMspace space;
    uint32_t bytes, live;
    int uid, state, warm;
} Arena;
typedef struct HeapStats {
    uint32_t cpu_committed, cpu_peak, main_committed, main_limit;
    uint32_t arena_count, live_allocations, warm_bytes, release_failures;
    uint32_t in_use;
    int32_t last_error;
} HeapStats;
static SceKernelLwMutexWork heap_mutex;
static Arena arenas[ARENA_LIMIT];
static Arena *ordered[ARENA_LIMIT], *hint;
static uint32_t ordered_count, cpu_committed, cpu_peak, arena_count;
static uint32_t release_failures;
static int initialized;
static _Atomic(int32_t) last_error;
static _Atomic(uint32_t) main_committed;
static _Atomic(uint32_t) main_limit = UINT32_MAX;

/* Only these allocation-free atomics are shared with native GPU block owners.
 * A reservation precedes kernel allocation; failure refunds it, successful
 * physical free refunds it exactly once. Logical heap objects never reserve. */
int vn_main_reserve(uint32_t bytes) {
    uint32_t used = atomic_load_explicit(&main_committed, memory_order_relaxed);
    for (;;) {
        uint32_t limit = atomic_load_explicit(&main_limit, memory_order_acquire);
        if (used > limit || bytes > limit - used) return 0;
        if (atomic_compare_exchange_weak_explicit(&main_committed, &used, used + bytes,
                memory_order_acq_rel, memory_order_relaxed)) return 1;
    }
}
void vn_main_refund(uint32_t bytes) {
    atomic_fetch_sub_explicit(&main_committed, bytes, memory_order_acq_rel);
}
uint32_t vn_main_capacity(void) {
    return atomic_load_explicit(&main_limit, memory_order_acquire);
}
uint32_t vn_main_committed(void) {
    return atomic_load_explicit(&main_committed, memory_order_acquire);
}
static int lock_heap(void) {
    int code = sceKernelLockLwMutex(&heap_mutex, 1, NULL);
    if (code < 0) last_error = code;
    return code;
}
static void unlock_heap(void) { sceKernelUnlockLwMutex(&heap_mutex, 1); }
static void forget_index(Arena *arena) {
    for (uint32_t i = 0; i < ordered_count; ++i) {
        if (ordered[i] == arena) {
            for (uint32_t j = i + 1; j < ordered_count; ++j) ordered[j - 1] = ordered[j];
            --ordered_count;
            break;
        }
    }
    if (hint == arena) hint = NULL;
}
static void insert_index(Arena *arena) {
    uint32_t i = ordered_count;
    while (i && (uintptr_t)ordered[i - 1]->base > (uintptr_t)arena->base) {
        ordered[i] = ordered[i - 1];
        --i;
    }
    ordered[i] = arena;
    ++ordered_count;
}
/* The sorted index contains only READY arenas. C allocation callers supply a
 * live pointer from this process heap; integer range lookup never dereferences
 * caller memory, and its result stays protected until the operation ends. */
static Arena *owner(void *pointer) {
    uintptr_t p = (uintptr_t)pointer;
    uint32_t low = 0, high = ordered_count;
    while (low < high) {
        uint32_t middle = low + (high - low) / 2;
        if ((uintptr_t)ordered[middle]->base <= p) low = middle + 1;
        else high = middle;
    }
    if (!low) return NULL;
    Arena *a = ordered[low - 1];
    return p - (uintptr_t)a->base < a->bytes ? a : NULL;
}
static int release_arena(Arena *a) {
    /* Destroy only empty mspaces (or final CRT owners). A failed physical free
     * leaves a RETIRED descriptor, UID and full commitment for a later retry. */
    if (a->state == READY) {
        forget_index(a);
        sceClibMspaceDestroy(a->space);
        a->space = NULL;
        a->state = RETIRED;
    }
    int code = sceKernelFreeMemBlock(a->uid);
    if (code < 0) {
        last_error = code;
        ++release_failures;
        return 0;
    }
    vn_main_refund(a->bytes);
    cpu_committed -= a->bytes;
    --arena_count;
    a->state = 0;
    a->base = NULL;
    return 1;
}
static void retry_retired(void) {
    for (uint32_t i = 0; i < ARENA_LIMIT; ++i)
        if (arenas[i].state == RETIRED) release_arena(&arenas[i]);
}
static Arena *new_arena(uint32_t request, uint32_t alignment, int warm) {
    uint64_t extent = (uint64_t)request + alignment + PAGE;
    if (extent > UINT32_MAX - (PAGE - 1)) return NULL;
    uint32_t bytes = ((uint32_t)extent + PAGE - 1) & ~(PAGE - 1);
    if (bytes < QUANTUM) bytes = QUANTUM;
    Arena *a = NULL;
    for (uint32_t i = 0; i < ARENA_LIMIT; ++i)
        if (!arenas[i].state) { a = &arenas[i]; break; }
    if (!a || !vn_main_reserve(bytes)) return NULL;
    int uid = sceKernelAllocMemBlock("process-heap", SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_RW,
        bytes, NULL);
    if (uid < 0) { last_error = uid; vn_main_refund(bytes); return NULL; }
    *a = (Arena){ .bytes = bytes, .uid = uid, .state = RETIRED, .warm = warm };
    ++arena_count;
    cpu_committed += bytes;
    if (cpu_committed > cpu_peak) cpu_peak = cpu_committed;
    int code = sceKernelGetMemBlockBase(uid, &a->base);
    if (code < 0 || !a->base) {
        last_error = code < 0 ? code : -1;
        release_arena(a);
        return NULL;
    }
    a->space = sceClibMspaceCreate(a->base, bytes);
    if (!a->space) { release_arena(a); return NULL; }
    a->state = READY;
    insert_index(a);
    return a;
}
static int observe_main_limit(void) {
    SceKernelFreeMemorySizeInfo info = {0};
    info.size = sizeof(info);
    int code = sceKernelGetFreeMemorySize(&info);
    if (code < 0) { last_error = code; return code; }
    uint64_t total = (uint64_t)atomic_load_explicit(&main_committed, memory_order_relaxed)
        + (uint32_t)info.size_user;
    if (total > UINT32_MAX) return -1;
    atomic_store_explicit(&main_limit, (uint32_t)total, memory_order_release);
    return 0;
}
int vn_heap_init(void) {
    if (initialized) return 0;
    int code = sceKernelCreateLwMutex(&heap_mutex, "process-heap", 2, 0, NULL);
    if (code < 0) return code;
    initialized = 1;
    if (observe_main_limit() < 0 || !new_arena(1, 8, 1)) return -1;
    return 0;
}
int vn_heap_configure(void) {
    if (!initialized) return -1;
    int locked = lock_heap();
    if (locked < 0) return locked;
    int code = observe_main_limit();
    unlock_heap();
    return code;
}
void vn_heap_shutdown(void) {
    if (!initialized) return;
    if (lock_heap() < 0) return;
    for (uint32_t i = 0; i < ARENA_LIMIT; ++i)
        if (arenas[i].state) release_arena(&arenas[i]);
    unlock_heap();
    sceKernelDeleteLwMutex(&heap_mutex);
    initialized = 0;
}
static int alignment_valid(uint32_t alignment) {
    return alignment && !(alignment & (alignment - 1));
}
static void *in_arena(Arena *a, uint32_t alignment, uint32_t size) {
    return alignment <= 8 ? sceClibMspaceMalloc(a->space, size)
        : sceClibMspaceMemalign(a->space, alignment, size);
}
static void *allocate_locked(uint32_t size, uint32_t alignment) {
    if (!size) size = 1; /* Preserve newlib minimum-object, including realloc(p,0). */
    if (!alignment_valid(alignment)) return NULL;
    if (alignment < 8) alignment = 8;
    Arena *a = hint;
    void *p = a ? in_arena(a, alignment, size) : NULL;
    if (!p) {
        for (uint32_t i = 0; i < ordered_count; ++i) {
            Arena *candidate = ordered[i];
            if (candidate == a) continue;
            p = in_arena(candidate, alignment, size);
            if (p) { a = candidate; break; }
        }
    }
    if (!p) {
        retry_retired();
        a = new_arena(size, alignment, 0);
        if (!a) return NULL;
        p = in_arena(a, alignment, size);
        if (!p) { release_arena(a); return NULL; }
    }
    ++a->live;
    hint = a;
    return p;
}
static void free_locked(Arena *a, void *p) {
    sceClibMspaceFree(a->space, p);
    --a->live;
    if (!a->warm && !a->live && sceClibMspaceIsHeapEmpty(a->space)) release_arena(a);
}
void *vn_heap_memalign(uint32_t alignment, uint32_t size) {
    if (!initialized) return NULL;
    if (lock_heap() < 0) return NULL;
    void *p = allocate_locked(size, alignment);
    unlock_heap();
    return p;
}
void *vn_heap_malloc(uint32_t size) { return vn_heap_memalign(8, size); }
void vn_heap_free(void *p) {
    if (!p || !initialized) return;
    if (lock_heap() < 0) return;
    Arena *a = owner(p);
    if (a) free_locked(a, p);
    unlock_heap();
}
void *vn_heap_calloc(uint32_t count, uint32_t size) {
    if (size && count > UINT32_MAX / size) return NULL;
    uint32_t bytes = count * size;
    void *p = vn_heap_malloc(bytes);
    /* This new object is private until return, so zeroing needs no heap lock. */
    if (p) sceClibMemset(p, 0, bytes);
    return p;
}
void *vn_heap_realloc(void *p, uint32_t size) {
    if (!p) return vn_heap_malloc(size);
    if (!initialized) return NULL;
    if (!size) size = 1;
    if (lock_heap() < 0) return NULL;
    Arena *a = owner(p);
    void *next = a ? sceClibMspaceRealloc(a->space, p, size) : NULL;
    if (a && !next) {
        /* The old allocation stays live through both admission and allocation.
         * Copy/free happen only after a complete replacement has been obtained. */
        next = allocate_locked(size, 8);
        if (next) {
            uint32_t old = sceClibMspaceMallocUsableSize(p);
            sceClibMemcpy(next, p, old < size ? old : size);
            free_locked(a, p);
        }
    }
    unlock_heap();
    return next;
}
uint32_t vn_heap_usable_size(void *p) {
    if (!p || !initialized) return 0;
    if (lock_heap() < 0) return 0;
    uint32_t size = owner(p) ? sceClibMspaceMallocUsableSize(p) : 0;
    unlock_heap();
    return size;
}
int vn_heap_trim(void) {
    if (!initialized) return 0;
    if (lock_heap() < 0) return 0;
    uint32_t before = cpu_committed;
    retry_retired();
    int changed = before != cpu_committed;
    unlock_heap();
    return changed;
}
int vn_heap_stats(HeapStats *out) {
    if (!out || !initialized) return -1;
    int locked = lock_heap();
    if (locked < 0) return locked;
    uint32_t live = 0, warm = 0, in_use = 0;
    for (uint32_t i = 0; i < ARENA_LIMIT; ++i) if (arenas[i].state) {
        live += arenas[i].live;
        if (arenas[i].warm) warm += arenas[i].bytes;
        if (arenas[i].state == READY) {
            SceClibMspaceStats native = {0};
            sceClibMspaceMallocStatsFast(arenas[i].space, &native);
            in_use += native.current_in_use;
        }
    }
    *out = (HeapStats){ cpu_committed, cpu_peak,
        atomic_load_explicit(&main_committed, memory_order_relaxed),
        atomic_load_explicit(&main_limit, memory_order_relaxed), arena_count, live,
        warm, release_failures, in_use, last_error };
    unlock_heap();
    return 0;
}
void vita_native_process_heap_anchor(void) {}

#ifndef VITA_HEAP_TEST
#include <errno.h>
#include <reent.h>
#include <malloc.h>
_Static_assert(sizeof(size_t) == 4, "Vita allocator size_t ABI");
/* Both public and reentrant entry points must be supplied: newlib stdio uses
 * _malloc_r directly. All sizes are 32-bit on Vita; normalization happens above. */
void *_malloc_r(struct _reent *r, size_t size) {
    void *p = vn_heap_malloc(size);
    if (!p && r) r->_errno = ENOMEM;
    return p;
}
void *_memalign_r(struct _reent *r, size_t align, size_t size) {
    void *p = vn_heap_memalign(align, size);
    if (!p && r) r->_errno = alignment_valid(align) ? ENOMEM : EINVAL;
    return p;
}
void *_calloc_r(struct _reent *r, size_t n, size_t size) {
    void *p = vn_heap_calloc(n, size);
    if (!p && r) r->_errno = ENOMEM;
    return p;
}
void *_realloc_r(struct _reent *r, void *old, size_t size) {
    void *p = vn_heap_realloc(old, size);
    if (!p && r) r->_errno = ENOMEM;
    return p;
}
void _free_r(struct _reent *r, void *p) { (void)r; vn_heap_free(p); }
size_t _malloc_usable_size_r(struct _reent *r, void *p) { (void)r; return vn_heap_usable_size(p); }
void *malloc(size_t n) { return _malloc_r(_REENT, n); }
void *calloc(size_t n, size_t s) { return _calloc_r(_REENT, n, s); }
void *realloc(void *p, size_t n) { return _realloc_r(_REENT, p, n); }
void free(void *p) { vn_heap_free(p); }
void *memalign(size_t a, size_t n) { return _memalign_r(_REENT, a, n); }
size_t malloc_usable_size(void *p) { return vn_heap_usable_size(p); }
void cfree(void *p) { vn_heap_free(p); }
void _cfree_r(struct _reent *r, void *p) { _free_r(r, p); }
int posix_memalign(void **out, size_t a, size_t n) {
    if (!out || a < sizeof(void *) || !alignment_valid(a)) return EINVAL;
    void *p = vn_heap_memalign(a, n);
    if (!p) return ENOMEM;
    *out = p;
    return 0;
}
void *aligned_alloc(size_t a, size_t n) {
    if (!alignment_valid(a) || n % a) { errno = EINVAL; return NULL; }
    return _memalign_r(_REENT, a, n);
}
void *_valloc_r(struct _reent *r, size_t n) { return _memalign_r(r, PAGE, n); }
void *valloc(size_t n) { return _valloc_r(_REENT, n); }
void *_pvalloc_r(struct _reent *r, size_t n) {
    if (n > UINT32_MAX - PAGE + 1) { if (r) r->_errno = ENOMEM; return NULL; }
    return _memalign_r(r, PAGE, (n + PAGE - 1) & ~(PAGE - 1));
}
void *pvalloc(size_t n) { return _pvalloc_r(_REENT, n); }
int _malloc_trim_r(struct _reent *r, size_t pad) { (void)r; (void)pad; return vn_heap_trim(); }
int malloc_trim(size_t pad) { return _malloc_trim_r(_REENT, pad); }
int _mallopt_r(struct _reent *r, int p, int v) { (void)r; (void)p; (void)v; return 0; }
int mallopt(int p, int v) { return _mallopt_r(_REENT, p, v); }
struct mallinfo _mallinfo_r(struct _reent *r) {
    (void)r;
    HeapStats stats = {0};
    vn_heap_stats(&stats);
    /* System mspaces expose capacity/usage, not newlib bin statistics. */
    struct mallinfo info = {0};
    info.arena = stats.cpu_committed;
    info.hblks = stats.arena_count;
    info.hblkhd = stats.cpu_committed;
    info.uordblks = stats.in_use;
    info.fordblks = stats.cpu_committed - stats.in_use;
    return info;
}
struct mallinfo mallinfo(void) { return _mallinfo_r(_REENT); }
void _malloc_stats_r(struct _reent *r) {
    (void)r;
    HeapStats stats = {0};
    vn_heap_stats(&stats);
    /* Explicit diagnostics are outside the allocator critical section. */
    sceClibPrintf("process heap: committed=%u in_use=%u arenas=%u\n",
        stats.cpu_committed, stats.in_use, stats.arena_count);
}
void malloc_stats(void) { _malloc_stats_r(_REENT); }
void _mstats_r(struct _reent *r, char *s) { (void)s; _malloc_stats_r(r); }
void mstats(char *s) { _mstats_r(_REENT, s); }
void __malloc_lock(struct _reent *r) { (void)r; if (lock_heap() < 0) sceClibAbort(); }
void __malloc_unlock(struct _reent *r) { (void)r; unlock_heap(); }
void _init_vita_heap(void) { if (vn_heap_init() < 0) sceClibAbort(); }
void _free_vita_heap(void) { vn_heap_shutdown(); }
void _init_vita_malloc(void) {}
void _free_vita_malloc(void) {}
unsigned int _get_vita_heap_size(void) { HeapStats s = {0}; vn_heap_stats(&s); return s.cpu_committed; }
void *_sbrk_r(struct _reent *r, ptrdiff_t n) {
    (void)n;
    if (r) r->_errno = ENOMEM;
    return (void *)-1;
}
#endif
