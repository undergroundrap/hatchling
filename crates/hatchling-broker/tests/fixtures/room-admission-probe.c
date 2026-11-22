/* Benign kernel observations at the actual Room supervisor exec boundary. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
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

    /* Its own network and IPC namespaces: neither is the host's (the
     * test's, passed in as their inodes). Its having no network rests on
     * the first; the second keeps it from the host's System V IPC. */
    if (stat("/proc/self/ns/net", &info) || info.st_ino == HOST_NET_NS)
        return 49;
    if (stat("/proc/self/ns/ipc", &info) || info.st_ino == HOST_IPC_NS)
        return 50;

    /* Its own host name and cgroup root (#572): the machine's name, and
     * where the Room sits in the host's cgroups, aren't its to read. */
    if (stat("/proc/self/ns/uts", &info) || info.st_ino == HOST_UTS_NS)
        return 51;
    char name[65] = {0};
    if (gethostname(name, sizeof name - 1) || strcmp(name, ROOM_HOST_NAME))
        return 52;
    if (stat("/proc/self/ns/cgroup", &info) || info.st_ino == HOST_CGROUP_NS)
        return 53;
    char cgroup[256] = {0};
    int cgroup_fd = open("/proc/self/cgroup", O_RDONLY);
    if (cgroup_fd < 0 || read(cgroup_fd, cgroup, sizeof cgroup - 1) <= 0 || close(cgroup_fd) ||
        strcmp(cgroup, "0::/\n"))
        return 54;

    /* No network: loopback is its only interface, and the filter
     * refuses any socket but a Unix one, as the Room's profile says. */
    char devices[4096] = {0};
    int devices_fd = open("/proc/self/net/dev", O_RDONLY);
    if (devices_fd < 0 || read(devices_fd, devices, sizeof devices - 1) <= 0 || close(devices_fd))
        return 55;
    int interfaces = 0;
    for (char *line = strtok(devices, "\n"); line; line = strtok(NULL, "\n")) {
        if (!strchr(line, ':'))
            continue;
        while (*line == ' ')
            ++line;
        if (strncmp(line, "lo:", 3))
            return 55;
        ++interfaces;
    }
    if (interfaces != 1)
        return 55;
    if (socket(AF_INET, SOCK_STREAM, 0) != -1 || errno != EAFNOSUPPORT ||
        socket(AF_INET6, SOCK_DGRAM, 0) != -1 || errno != EAFNOSUPPORT ||
        socket(AF_NETLINK, SOCK_RAW, 0) != -1 || errno != EAFNOSUPPORT)
        return 56;

    /* No descriptor of the host's but the supervisor's socket: every one
     * open is 0 to 3, or the one this reads them through. */
    DIR *open_fds = opendir("/proc/self/fd");
    if (!open_fds)
        return 57;
    for (struct dirent *entry; (entry = readdir(open_fds));) {
        if (entry->d_name[0] == '.')
            continue;
        int fd = atoi(entry->d_name);
        if (fd > 3 && fd != dirfd(open_fds))
            return 57;
    }
    if (closedir(open_fds))
        return 57;

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
        /* One whole dispatch frame. The spawner keeps its end open for
         * the next command, so there is no end of input to wait for. */
        unsigned char head[4];
        char dispatch[512];
        size_t have = 0;
        while (have < sizeof head) {
            ssize_t received = read(3, head + have, sizeof head - have);
            if (received < 0 && errno == EINTR)
                continue;
            if (received <= 0)
                return 47;
            have += (size_t)received;
        }
        size_t left = ((size_t)head[0] << 24) | ((size_t)head[1] << 16)
            | ((size_t)head[2] << 8) | (size_t)head[3];
        while (left) {
            ssize_t received = read(3, dispatch, left < sizeof dispatch ? left : sizeof dispatch);
            if (received < 0 && errno == EINTR)
                continue;
            if (received <= 0)
                return 47;
            left -= (size_t)received;
        }
        /* An answer to something that was never asked. */
        static const char stale[] = "{\"say\":\"started\",\"nonce\":\""
            "0000000000000000000000000000000000000000000000000000000000000000\"}";
        static const unsigned char size[] = {0, 0, 0, sizeof stale - 1};
        if (write(3, size, sizeof size) != sizeof size
            || write(3, stale, sizeof stale - 1) != sizeof stale - 1)
            return 48;
    }
    return 0;
}
