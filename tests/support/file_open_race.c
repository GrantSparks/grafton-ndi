/* Insert a collision immediately before the native File SDK's open64. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, int);
        va_end(args);
    }
    int (*native_open)(const char *, int, ...) = dlsym(RTLD_NEXT, "open64");
    const char *target = getenv("GRAFTON_FILE_RACE_TARGET");
    static atomic_int inserted = 0;
    if (target && !strcmp(path, target) && (flags & O_CREAT) && !atomic_exchange(&inserted, 1)) {
        const char *victim = getenv("GRAFTON_FILE_RACE_SYMLINK");
        if (victim) {
            if (syscall(SYS_symlinkat, victim, AT_FDCWD, path) != 0) abort();
        } else {
            int fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY|O_CREAT|O_EXCL, 0600);
            if (fd < 0) abort();
            const char sentinel[] = "GRAFTON_RACE_SENTINEL\n";
            if (write(fd, sentinel, sizeof(sentinel)-1) != sizeof(sentinel)-1) abort();
            close(fd);
        }
    }
    return native_open(path, flags, mode);
}
