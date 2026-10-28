#ifndef VITA_HEAP_MOCK_H
#define VITA_HEAP_MOCK_H
#include <stdint.h>
#include <stddef.h>
#ifdef _WIN32
#include <windows.h>
typedef CRITICAL_SECTION SceKernelLwMutexWork;
#else
#include <pthread.h>
typedef pthread_mutex_t SceKernelLwMutexWork;
#endif
typedef void *SceClibMspace;
typedef struct { uint32_t capacity, unk, peak_in_use, current_in_use; } SceClibMspaceStats;
typedef struct { uint32_t size; int32_t size_user, size_cdram, size_phycont; } SceKernelFreeMemorySizeInfo;
#define SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_RW 0x0c20d060
int sceKernelCreateLwMutex(SceKernelLwMutexWork *, const char *, int, int, void *);
int sceKernelLockLwMutex(SceKernelLwMutexWork *, int, void *);
int sceKernelUnlockLwMutex(SceKernelLwMutexWork *, int);
int sceKernelDeleteLwMutex(SceKernelLwMutexWork *);
int sceKernelAllocMemBlock(const char *, int, uint32_t, void *);
int sceKernelGetMemBlockBase(int, void **);
int sceKernelFreeMemBlock(int);
int sceKernelGetFreeMemorySize(SceKernelFreeMemorySizeInfo *);
SceClibMspace sceClibMspaceCreate(void *, uint32_t);
void sceClibMspaceDestroy(SceClibMspace);
void sceClibMspaceMallocStatsFast(SceClibMspace, SceClibMspaceStats *);
void *sceClibMspaceMalloc(SceClibMspace, uint32_t);
void *sceClibMspaceMemalign(SceClibMspace, uint32_t, uint32_t);
void *sceClibMspaceRealloc(SceClibMspace, void *, uint32_t);
void sceClibMspaceFree(SceClibMspace, void *);
int sceClibMspaceIsHeapEmpty(SceClibMspace);
uint32_t sceClibMspaceMallocUsableSize(void *);
void *sceClibMemset(void *, int, uint32_t);
void *sceClibMemcpy(void *, const void *, uint32_t);
#endif
