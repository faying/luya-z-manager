/* Compat shim: symbols present in musl 1.2.5+ but missing from the
 * device's musl 1.2.4 (OpenWrt 23.05). Preloaded via LD_PRELOAD. */
#define _GNU_SOURCE
#include <sys/syscall.h>
#include <unistd.h>

int statx(int dirfd, const char *path, int flags, unsigned mask, void *buf) {
    return syscall(291, dirfd, path, flags, mask, buf);  /* __NR_statx aarch64 */
}
