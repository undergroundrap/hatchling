/* A deterministic workload for the verified-image fixture: the jobs the
 * installed Room tests run. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

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

/* Fills the session scratch until it takes no more. What the supervisor
 * keeps there counts against the same cap, so the fill ends a little
 * short of it: never past it, and never far short. */
static int fill_scratch(void)
{
    int fd = open("/session/fill", O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0)
        return 9;
    unsigned blocks = 0;
    while (blocks < 513 && !send_bytes(fd, zeros, sizeof(zeros)))
        ++blocks;
    int error = errno;
    if (close(fd) < 0 || blocks < 500 || blocks > 512 || error != ENOSPC)
        return 11;
    return send_bytes(STDERR_FILENO, "ENOSPC", 6) ? 12 : 1;
}

/* Where the job was started, and everything in its environment. */
static int surroundings(void)
{
    char cwd[128];
    if (!getcwd(cwd, sizeof(cwd)))
        return 2;
    if (send_bytes(STDOUT_FILENO, cwd, strlen(cwd)))
        return 4;
    for (char **entry = environ; *entry; ++entry) {
        if (send_bytes(STDOUT_FILENO, "\n", 1)
            || send_bytes(STDOUT_FILENO, *entry, strlen(*entry)))
            return 4;
    }
    return 0;
}

/* What the job was given to read, written back. */
static int input(void)
{
    char bytes[4096];
    for (;;) {
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0)
            return 19;
        if (count == 0)
            return 0;
        if (send_bytes(STDOUT_FILENO, bytes, (size_t)count))
            return 4;
    }
}

/* The size of a file, in decimal. */
static int size_of(const char *path)
{
    struct stat info;
    if (stat(path, &info))
        return 18;
    char text[32];
    int length = snprintf(text, sizeof(text), "%lld", (long long)info.st_size);
    return send_bytes(STDOUT_FILENO, text, (size_t)length) ? 4 : 0;
}

int main(int argc, char **argv)
{
    if (argc == 3 && !strcmp(argv[1], "seed"))
        return seed(argv[2]);
    if (argc == 3 && !strcmp(argv[1], "size"))
        return size_of(argv[2]);
    if (argc != 2)
        return 13;
    if (!strcmp(argv[1], "resume"))
        return resume();
    /* The scratch an earlier job left is there only in the same Room. */
    if (!strcmp(argv[1], "kept"))
        return access("/session/marker", F_OK) ? 18 : 0;
    if (!strcmp(argv[1], "output"))
        return send_bytes(STDERR_FILENO, zeros, 131072)
            || send_bytes(STDOUT_FILENO, zeros, 4096);
    if (!strcmp(argv[1], "fill-scratch"))
        return fill_scratch();
    if (!strcmp(argv[1], "surroundings"))
        return surroundings();
    if (!strcmp(argv[1], "input"))
        return input();
    /* Runs until it is stopped: its first process, silent. */
    if (!strcmp(argv[1], "sleep")) {
        if (store("timeout-marker", "started") && errno != EEXIST)
            return 15;
        struct timespec interval = { .tv_sec = 60 };
        return nanosleep(&interval, 0) ? 16 : 0;
    }
    /* Ends at once, leaving a child that holds its output open. */
    if (!strcmp(argv[1], "wait")) {
        pid_t child = fork();
        if (child < 0)
            return 14;
        if (child > 0)
            return 0;
        if (store("left-marker", "started"))
            return 15;
        struct timespec interval = { .tv_sec = 60 };
        return nanosleep(&interval, 0) ? 16 : 0;
    }
    return 17;
}
