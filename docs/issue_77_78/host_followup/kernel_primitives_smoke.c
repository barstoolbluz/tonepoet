// Independent Linux ABI smoke test for the issue 77/78 follow-up. It does NOT
// compile or execute TonePoet. Compile with: cc -O2 -Wall -Wextra -Werror ...
#define _GNU_SOURCE
#include <errno.h>
#include <linux/capability.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define REQUIRE(x) do { if (!(x)) { \
    fprintf(stderr, "FAIL %s at %s:%d: %s\n", #x, __FILE__, __LINE__, strerror(errno)); \
    exit(1); \
} } while (0)

static int get_caps(struct __user_cap_data_struct data[2]) {
    struct __user_cap_header_struct h = { .version = _LINUX_CAPABILITY_VERSION_3, .pid = 0 };
    return (int)syscall(SYS_capget, &h, data);
}

static void socket_poll_test(void) {
    int sv[2];
    REQUIRE(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) == 0);
    REQUIRE(write(sv[1], "R", 1) == 1);
    struct pollfd p = { .fd = sv[0], .events = POLLIN };
    REQUIRE(poll(&p, 1, 10) == 1);
    REQUIRE(p.revents & POLLIN);
    char ch = 0;
    REQUIRE(read(sv[0], &ch, 1) == 1 && ch == 'R');
    REQUIRE(close(sv[1]) == 0);
    p.revents = 0;
    REQUIRE(poll(&p, 1, 10) == 1);
    REQUIRE(p.revents & POLLHUP);
    REQUIRE(close(sv[0]) == 0);
    puts("socket lifecycle wake/HUP: PASS");
}

static int capabilities_after_exec(void) {
    struct __user_cap_data_struct data[2] = {0};
    REQUIRE(get_caps(data) == 0);
    const unsigned bit = 1u << CAP_CHOWN;
    REQUIRE(((data[0].effective | data[0].permitted | data[0].inheritable) & bit) == 0);
    REQUIRE(prctl(PR_CAPBSET_READ, CAP_CHOWN, 0, 0, 0) == 0);
    REQUIRE(prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1);
    puts("capability drop/bounding/no_new_privs after exec: PASS");
    return 0;
}

static void capability_drop_test(const char *exe) {
    struct __user_cap_data_struct source[2] = {0};
    REQUIRE(get_caps(source) == 0);
    const unsigned chown = 1u << CAP_CHOWN;
    const unsigned setpcap = 1u << CAP_SETPCAP;
    if (!(source[0].effective & chown) || !(source[0].effective & setpcap)) {
        puts("capability drop/exec: SKIP (test process lacks CAP_CHOWN or CAP_SETPCAP)");
        return;
    }
    pid_t pid = fork();
    REQUIRE(pid >= 0);
    if (pid == 0) {
        REQUIRE(prctl(PR_CAPBSET_DROP, CAP_CHOWN, 0, 0, 0) == 0);
        struct __user_cap_header_struct h = {
            .version = _LINUX_CAPABILITY_VERSION_3, .pid = 0
        };
        struct __user_cap_data_struct data[2] = {0};
        REQUIRE(syscall(SYS_capget, &h, data) == 0);
        data[0].effective &= ~chown;
        data[0].permitted &= ~chown;
        data[0].inheritable &= ~chown;
        REQUIRE(syscall(SYS_capset, &h, data) == 0);
        REQUIRE(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0);
        char *const args[] = {(char *)exe, "verify", NULL};
        execv(exe, args);
        perror("execv");
        _exit(1);
    }
    int status = 0;
    REQUIRE(waitpid(pid, &status, 0) == pid);
    REQUIRE(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "verify") == 0)
        return capabilities_after_exec();
    if (argc != 1) {
        fprintf(stderr, "unexpected arguments\n");
        return 2;
    }
    socket_poll_test();
    capability_drop_test(argv[0]);
    return 0;
}
