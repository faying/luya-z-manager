/*
 * touchsim - synthetic input for testing the touch UI over SSH.
 *
 * Writes Linux evdev events straight into an input node, so the running UI
 * sees them exactly as if a finger (or a button) had been used.
 *
 *   touchsim NODE tap X Y
 *   touchsim NODE swipe X0 Y0 X1 Y1 [MS]      MS defaults to 300
 *   touchsim NODE key CODE [HOLD_MS]          HOLD_MS defaults to 60
 *
 * NODE is a path such as /dev/input/event3, or "auto" to use the first
 * /dev/input/event* that reports multi-touch X and Y axes. Coordinates are
 * raw panel coordinates (the U60 panel is mounted upside down, so a point
 * seen at (x, y) in a screenshot is (319 - x, 479 - y) here). KEY_POWER is
 * 116 and lives on /dev/input/event0; holding it 800 ms or more opens the
 * power menu.
 *
 * Build (static aarch64):
 *   zig cc -target aarch64-linux-musl -static -Os -o touchsim tools/touchsim.c
 *
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/time.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* The evdev wire format and the few codes used here (linux/input.h values);
 * spelled out so the file builds without kernel headers. */
struct evdev_record {
    struct timeval stamp;
    uint16_t type;
    uint16_t code;
    int32_t value;
};

enum {
    T_SYN = 0x00,
    T_KEY = 0x01,
    T_ABS = 0x03,
};

enum {
    C_SYN_REPORT = 0x00,
    C_BTN_TOUCH = 0x14a,
    C_MT_X = 0x35,
    C_MT_Y = 0x36,
    C_MT_TRACKING = 0x39,
};

#define FRAME_MS 16          /* swipe step, roughly one display frame */
#define MIN_SWIPE_STEPS 4
#define TAP_HOLD_MS 50
#define SWIPE_SETTLE_MS 30   /* pause at the end point before lifting */
#define NODE_PATH_MAX 288

static int out_fd = -1;

static void pause_ms(long ms)
{
    if (ms <= 0)
        return;
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR)
        ;
}

static void emit(uint16_t type, uint16_t code, int32_t value)
{
    struct evdev_record r = { .type = type, .code = code, .value = value };
    if (write(out_fd, &r, sizeof r) != (ssize_t)sizeof r)
        fprintf(stderr, "touchsim: write failed: %s\n", strerror(errno));
}

static void sync_frame(void) { emit(T_SYN, C_SYN_REPORT, 0); }

static void move_to(int x, int y)
{
    emit(T_ABS, C_MT_X, x);
    emit(T_ABS, C_MT_Y, y);
}

/* Finger down in slot 0 at (x, y), as one frame. */
static void finger_down(int x, int y)
{
    emit(T_ABS, C_MT_TRACKING, 0);
    move_to(x, y);
    emit(T_KEY, C_BTN_TOUCH, 1);
    sync_frame();
}

/* Finger up: no position, the contact just ends. */
static void finger_up(void)
{
    emit(T_KEY, C_BTN_TOUCH, 0);
    emit(T_ABS, C_MT_TRACKING, -1);
    sync_frame();
}

static int axis_bit(const unsigned char *mask, int bit)
{
    return (mask[bit / 8] >> (bit % 8)) & 1;
}

/* First event node with multi-touch X and Y; 0 on success. */
static int find_touch_node(char *path, size_t len)
{
    DIR *dir = opendir("/dev/input");
    if (!dir)
        return -1;
    int found = -1;
    struct dirent *ent;
    while (found != 0 && (ent = readdir(dir)) != NULL) {
        if (strncmp(ent->d_name, "event", 5) != 0)
            continue;
        char cand[NODE_PATH_MAX];
        snprintf(cand, sizeof cand, "/dev/input/%s", ent->d_name);
        int fd = open(cand, O_RDONLY | O_NONBLOCK);
        if (fd < 0)
            continue;
        unsigned char mask[16] = { 0 };   /* ABS codes 0..127 */
        /* EVIOCGBIT(EV_ABS, len), encoded the Linux way: read | size | 'E' | nr */
        unsigned long req = (2UL << 30) | ((unsigned long)sizeof mask << 16) | ('E' << 8) | (0x20 + T_ABS);
        if (ioctl(fd, req, mask) >= 0 && axis_bit(mask, C_MT_X) && axis_bit(mask, C_MT_Y)) {
            snprintf(path, len, "%s", cand);
            found = 0;
        }
        close(fd);
    }
    closedir(dir);
    return found;
}

static int num(const char *s, int *out)
{
    char *end;
    errno = 0;
    long v = strtol(s, &end, 10);
    if (errno || end == s || *end || v < -100000 || v > 100000)
        return -1;
    *out = (int)v;
    return 0;
}

/* Parse up to max numbers from argv[from..] into v (extra words are ignored);
 * optional ones that are absent keep v's preset. */
static int nums(char **argv, int argc, int from, int need, int max, int *v)
{
    int have = argc - from;
    if (have < need)
        return -1;
    if (have > max)
        have = max;
    for (int i = 0; i < have; i++)
        if (num(argv[from + i], &v[i]) != 0)
            return -1;
    return 0;
}

static int cmd_tap(const char *node, char **argv, int argc)
{
    int v[2];
    if (nums(argv, argc, 3, 2, 2, v) != 0)
        return -1;
    printf("tap %d,%d on %s\n", v[0], v[1], node);
    finger_down(v[0], v[1]);
    pause_ms(TAP_HOLD_MS);
    finger_up();
    return 0;
}

static int cmd_swipe(const char *node, char **argv, int argc)
{
    int v[5] = { 0, 0, 0, 0, 300 };
    if (nums(argv, argc, 3, 4, 5, v) != 0 || v[4] < 0)
        return -1;
    int x0 = v[0], y0 = v[1], x1 = v[2], y1 = v[3], ms = v[4];
    printf("swipe %d,%d -> %d,%d (%dms) on %s\n", x0, y0, x1, y1, ms, node);

    int steps = ms / FRAME_MS;
    if (steps < MIN_SWIPE_STEPS)
        steps = MIN_SWIPE_STEPS;
    long step_ms = (long)ms / steps;

    finger_down(x0, y0);
    for (int k = 1; k <= steps; k++) {
        pause_ms(step_ms);
        move_to(x0 + (x1 - x0) * k / steps, y0 + (y1 - y0) * k / steps);
        sync_frame();
    }
    pause_ms(SWIPE_SETTLE_MS);
    finger_up();
    return 0;
}

/* A key press and release. The UI lights the panel only from the power key,
 * so a headless check that starts with the screen dark needs this first. */
static int cmd_key(const char *node, char **argv, int argc)
{
    int v[2] = { 0, 60 };
    if (nums(argv, argc, 3, 1, 2, v) != 0 || v[0] < 0 || v[0] > 0x2ff || v[1] < 0)
        return -1;
    printf("key %d (%dms) on %s\n", v[0], v[1], node);
    emit(T_KEY, (uint16_t)v[0], 1);
    sync_frame();
    pause_ms(v[1]);
    emit(T_KEY, (uint16_t)v[0], 0);
    sync_frame();
    return 0;
}

static const struct {
    const char *name;
    int (*run)(const char *node, char **argv, int argc);
    const char *args;
} commands[] = {
    { "tap", cmd_tap, "X Y" },
    { "swipe", cmd_swipe, "X0 Y0 X1 Y1 [MS]" },
    { "key", cmd_key, "CODE [HOLD_MS]   (KEY_POWER = 116, usually on /dev/input/event0)" },
};
#define N_COMMANDS (sizeof commands / sizeof commands[0])

static int usage(void)
{
    for (size_t i = 0; i < N_COMMANDS; i++)
        fprintf(stderr, "%s touchsim NODE|auto %s %s\n", i ? "      " : "usage:", commands[i].name,
                commands[i].args);
    return 1;
}

int main(int argc, char **argv)
{
    if (argc < 3)
        return usage();

    size_t which = N_COMMANDS;
    for (size_t i = 0; i < N_COMMANDS; i++)
        if (strcmp(argv[2], commands[i].name) == 0)
            which = i;
    if (which == N_COMMANDS) {
        fprintf(stderr, "touchsim: unknown action '%s'\n", argv[2]);
        return usage();
    }

    char node[NODE_PATH_MAX];
    if (strcmp(argv[1], "auto") == 0) {
        if (find_touch_node(node, sizeof node) != 0) {
            fprintf(stderr, "touchsim: no multi-touch input node under /dev/input\n");
            return 1;
        }
    } else {
        snprintf(node, sizeof node, "%s", argv[1]);
    }

    out_fd = open(node, O_WRONLY);
    if (out_fd < 0) {
        fprintf(stderr, "touchsim: cannot open %s: %s\n", node, strerror(errno));
        return 1;
    }
    int rc = commands[which].run(node, argv, argc);
    close(out_fd);
    if (rc != 0) {
        fprintf(stderr, "touchsim: bad arguments for '%s'\n", commands[which].name);
        return usage();
    }
    return 0;
}
