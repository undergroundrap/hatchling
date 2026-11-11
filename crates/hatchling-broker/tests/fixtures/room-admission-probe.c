/* Benign kernel observations at the actual Room supervisor exec boundary. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

int main(int argc, char **argv, char **environment) {
    (void)argc;
    (void)argv;
    if (getpid() != 2 || getppid() != 1)
        return 31;

    uid_t uid, effective_uid, saved_uid;
    gid_t gid, effective_gid, saved_gid;
    if (getresuid(&uid, &effective_uid, &saved_uid) || uid != ROOM_UID ||
        effective_uid != uid || saved_uid != uid ||
        getresgid(&gid, &effective_gid, &saved_gid) || gid != ROOM_GID ||
        effective_gid != gid || saved_gid != gid || getgroups(0, NULL) != 0)
        return 32;

    struct stat info;
    if (stat("/session", &info) || info.st_uid != uid || info.st_gid != gid ||
        (info.st_mode & 0777) != 0700 || *environment != NULL)
        return 33;

    for (int fd = 0; fd < 3; ++fd) {
        if (fstat(fd, &info) || !S_ISCHR(info.st_mode) || info.st_rdev != makedev(1, 3))
            return 34;
    }
    if (fstat(3, &info) || !S_ISSOCK(info.st_mode))
        return 35;
    if (fcntl(57, F_GETFD) != -1 || errno != EBADF)
        return 36;

    /* The Room policy specifically reports ENOSYS for this call. */
    if (memfd_create("room-filter-witness", MFD_CLOEXEC) != -1 || errno != ENOSYS)
        return 37;

    char status[4096] = {0};
    int status_fd = open("/proc/self/status", O_RDONLY);
    if (status_fd < 0 || read(status_fd, status, sizeof status - 1) <= 0 || close(status_fd) ||
        !strstr(status, "NoNewPrivs:\t1\n") ||
        !strstr(status, "CapPrm:\t0000000000000000\n") ||
        !strstr(status, "CapEff:\t0000000000000000\n") ||
        !strstr(status, "CapAmb:\t0000000000000000\n"))
        return 38;

    if (ROOM_PROBE_MODE == 0) {
        const char *marker = "/workspace/admission-marker";
        int file = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (file >= 0) {
            if (write(file, "first", 5) != 5 || close(file))
                return 43;
        } else {
            char before[5];
            if (errno != EEXIST || (file = open(marker, O_RDWR | O_APPEND)) < 0 ||
                read(file, before, sizeof before) != (ssize_t)sizeof before ||
                memcmp(before, "first", sizeof before) || write(file, "second", 6) != 6 ||
                close(file))
                return 44;
        }
        if (!mkdir("/workspace/inbox/refused", 0700) || (errno != EACCES && errno != EROFS))
            return 45;
        if (!unlink("/workspace/root-owned-marker") || errno != EPERM)
            return 46;
    }

    static const char ready[] = "\0\0\0\x05ready";
    if (ROOM_PROBE_MODE == 1) {
        sleep(120);
        return 40;
    }
    if (write(3, ready, sizeof ready - 1) != (ssize_t)(sizeof ready - 1))
        return 39;
    if (ROOM_PROBE_MODE == 2) {
        sleep(120);
        return 41;
    }
    if (ROOM_PROBE_MODE == 3) {
        char dispatch[512];
        ssize_t received;
        do {
            received = read(3, dispatch, sizeof dispatch);
        } while (received > 0 || (received < 0 && errno == EINTR));
        if (received < 0)
            return 47;
        static const char stale[] = "[\"previous-dispatch\",{\"status\":{\"exited\":0},\"stdout\":[],\"stderr\":[],\"stdout_truncated\":false,\"stderr_truncated\":false}]";
        static const unsigned char size[] = {0, 0, 0, sizeof stale - 1};
        if (write(3, size, sizeof size) != sizeof size
            || write(3, stale, sizeof stale - 1) != sizeof stale - 1)
            return 48;
    }
    return 0;
}
