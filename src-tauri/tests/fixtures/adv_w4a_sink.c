/* W4a adversary decoy: a process that logs every catchable signal it is sent
 * to $SINK_LOG and otherwise sleeps, so a real `pkill`/`killall` can be aimed
 * at it with a real signal and the signal that arrived can be read back.
 * Compiled by tests/adv_w4a_signal_guard.rs, run under many names.
 *
 *   SINK_RES=r,e,s   setresuid(r, e, s): needs root; real, effective, saved ids
 *   SINK_COMM=name   prctl(PR_SET_NAME, name): the kernel's comm, no exec
 *   SINK_TITLE=text  overwrite the argument area in place (setproctitle)
 *   SINK_BLANK=1     zero the argument area: an empty /proc/<pid>/cmdline
 *   SINK_NODUMP=1    prctl(PR_SET_DUMPABLE, 0)
 *   SINK_ZOMBIE=1    leave an unreaped child behind
 *   SINK_SECS=n      exit after n seconds (default 300)
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

static int log_fd = -1;

static void on_signal(int sig) {
    char buf[16];
    int n = 0;
    buf[n++] = 'S';
    if (sig >= 100) buf[n++] = (char)('0' + sig / 100);
    if (sig >= 10) buf[n++] = (char)('0' + (sig / 10) % 10);
    buf[n++] = (char)('0' + sig % 10);
    buf[n++] = '\n';
    if (log_fd >= 0) {
        ssize_t r = write(log_fd, buf, (size_t)n);
        (void)r;
    }
}

int main(int argc, char **argv) {
    const char *log = getenv("SINK_LOG");
    if (log) log_fd = open(log, O_WRONLY | O_APPEND | O_CREAT, 0644);
    for (int s = 1; s < 65; s++) {
        if (s == SIGKILL || s == SIGSTOP) continue;
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = on_signal;
        sigemptyset(&sa.sa_mask);
        sigaction(s, &sa, NULL);
    }
    const char *res = getenv("SINK_RES");
    if (res) {
        unsigned r = 0, e = 0, s = 0;
        if (sscanf(res, "%u,%u,%u", &r, &e, &s) == 3) {
            if (setresuid(r, e, s) != 0) _exit(3);
        }
    }
    const char *comm = getenv("SINK_COMM");
    if (comm) prctl(PR_SET_NAME, comm, 0, 0, 0);
    if (getenv("SINK_NODUMP")) prctl(PR_SET_DUMPABLE, 0, 0, 0, 0);
    if (argc > 0 && (getenv("SINK_TITLE") || getenv("SINK_BLANK"))) {
        char *start = argv[0];
        char *end = argv[argc - 1] + strlen(argv[argc - 1]) + 1;
        size_t room = (size_t)(end - start);
        const char *title = getenv("SINK_TITLE");
        memset(start, 0, room);
        if (title) {
            size_t n = strlen(title);
            if (n > room - 1) n = room - 1;
            memcpy(start, title, n);
        }
    }
    if (getenv("SINK_ZOMBIE")) {
        pid_t child = fork();
        if (child == 0) _exit(0);
    }
    if (log_fd >= 0) {
        char ready[32];
        int n = snprintf(ready, sizeof ready, "R %d\n", (int)getpid());
        ssize_t r = write(log_fd, ready, (size_t)n);
        (void)r;
    }
    int secs = 300;
    if (getenv("SINK_SECS")) secs = atoi(getenv("SINK_SECS"));
    for (int i = 0; i < secs; i++) sleep(1);
    return 0;
}
