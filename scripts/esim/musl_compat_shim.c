/*
 * libmusl-compat.so -- loaded with LD_PRELOAD by /data/esim/lpac.sh.
 *
 * The Alpine libraries in the eSIM bundle were linked against a newer musl
 * that exports statx(); the device's OpenWrt musl 1.2.4 does not. This file
 * supplies that one symbol as a thin wrapper around the raw system call.
 *
 * <sys/stat.h> is left out on purpose: newer musl headers declare statx()
 * there with a struct pointer, which would clash with the plain pointer here.
 */
#define _GNU_SOURCE
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_statx
#define SYS_statx 291 /* aarch64 syscall number */
#endif

int statx(int dirfd, const char *pathname, int flags, unsigned int mask, void *statxbuf)
{
    return (int)syscall(SYS_statx, dirfd, pathname, flags, mask, statxbuf);
}
