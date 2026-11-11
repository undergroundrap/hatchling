/* A deterministic foreground workload for the verified-image fixture. */
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static char zeros[1024 * 1024];

static int send_bytes(int fd, const char *bytes, size_t size)
{
    while (size) {
        ssize_t count = write(fd, bytes, size);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0)
            return -1;
        bytes += count;
        size -= (size_t)count;
    }
    return 0;
}

static int store(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0)
        return -1;
    int result = send_bytes(fd, text, strlen(text));
    if (close(fd) < 0)
        return -1;
    return result;
}

static int seed(const char *text)
{
    char cwd[128];
    if (!getcwd(cwd, sizeof(cwd)))
        return 2;
    if (store("foreground-file", text) || store("/session/marker", "scratch"))
        return 3;
    if (send_bytes(STDOUT_FILENO, cwd, strlen(cwd))
        || send_bytes(STDERR_FILENO, "diagnostic", 10))
        return 4;
    return 23;
}

static int resume(void)
{
    if (access("/session/marker", F_OK) != -1 || errno != ENOENT)
        return 5;
    int fd = open("foreground-file", O_RDONLY);
    if (fd < 0)
        return 6;
    char bytes[256];
    ssize_t count = read(fd, bytes, sizeof(bytes));
    if (close(fd) < 0 || count <= 0)
        return 7;
    return send_bytes(STDOUT_FILENO, bytes, (size_t)count) ? 8 : 0;
}

static int fill_scratch(void)
{
    int fd = open("/session/fill", O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0)
        return 9;
    for (unsigned block = 0; block < 512; ++block) {
        if (send_bytes(fd, zeros, sizeof(zeros)))
            return 10;
    }
    ssize_t written = write(fd, "!", 1);
    int error = errno;
    if (close(fd) < 0 || written != -1 || error != ENOSPC)
        return 11;
    return send_bytes(STDERR_FILENO, "ENOSPC", 6) ? 12 : 1;
}

int main(int argc, char **argv)
{
    if (argc == 3 && !strcmp(argv[1], "seed"))
        return seed(argv[2]);
    if (argc != 2)
        return 13;
    if (!strcmp(argv[1], "resume"))
        return resume();
    if (!strcmp(argv[1], "output"))
        return send_bytes(STDERR_FILENO, zeros, 131072)
            || send_bytes(STDOUT_FILENO, zeros, 4096);
    if (!strcmp(argv[1], "fill-scratch"))
        return fill_scratch();
    if (!strcmp(argv[1], "wait")) {
        pid_t child = fork();
        if (child < 0)
            return 14;
        if (child > 0)
            return 0;
        if (store("timeout-marker", "started"))
            return 15;
        struct timespec interval = { .tv_sec = 60 };
        return nanosleep(&interval, 0) ? 16 : 0;
    }
    return 17;
}
