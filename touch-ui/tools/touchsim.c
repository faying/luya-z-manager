/* touchsim: fake finger and button input for the touch UI, driven over SSH.
 *
 * It writes evdev records directly into an input device node, so the UI that is
 * running cannot tell them from a real touch or key press.
 *
 * Usage:
 *   touchsim NODE tap X Y
 *   touchsim NODE swipe X0 Y0 X1 Y1 [MS]      (MS: 300 when left out)
 *   touchsim NODE key CODE [HOLD_MS]          (HOLD_MS: 60 when left out)
 * NODE: a device path like /dev/input/event3, or "auto" for the first
 * /dev/input/event* with multi-touch X/Y axes. X and Y are raw panel values; the
 * U60 panel sits upside down, so screenshot point (x, y) becomes (319 - x, 479 - y).
 * KEY_POWER is code 116 on /dev/input/event0; a hold of 800 ms or longer brings up
 * the power menu.
 *
 * Static aarch64 build:
 *   zig cc -target aarch64-linux-musl -static -Os -o touchsim tools/touchsim.c
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */

#include <dirent.h>   /* opendir, readdir */
#include <errno.h>    /* errno */
#include <fcntl.h>    /* open flags */
#include <stdint.h>   /* fixed-width ints */
#include <stdio.h>    /* printf, snprintf */
#include <stdlib.h>   /* strtol */
#include <string.h>   /* strcmp, strerror */
#include <sys/ioctl.h>/* ioctl */
#include <sys/time.h> /* struct timeval */
#include <sys/types.h>/* ssize_t */
#include <time.h>     /* nanosleep */
#include <unistd.h>   /* write, close */

/* struct input_event and the linux/input.h numbers this tool needs, copied in by
 * value so no kernel headers are required to build it. */
typedef struct {
    struct timeval when;
    uint16_t kind, what;
    int32_t val;
} ev_t;

#define EVK_SYN 0x00
#define EVK_KEY 0x01
#define EVK_ABS 0x03
#define SYN_REPORT_CODE 0x00
#define BTN_TOUCH_CODE 0x14a
#define ABS_MT_X_CODE 0x35
#define ABS_MT_Y_CODE 0x36
#define ABS_MT_TRACK_CODE 0x39
enum {
    STEP_MS = 16,        /* one swipe step ~ one frame on the panel */
    STEPS_AT_LEAST = 4,
    TAP_DOWN_MS = 50,
    SWIPE_REST_MS = 30,  /* finger rests on the end point before it lifts */
    PATH_CAP = 288,
};

static int dev = -1; /* the node all records go to */
static void sleep_ms(long ms) {
    struct timespec left = { ms / 1000, (ms % 1000) * 1000000L };
    if (ms > 0)
        while (nanosleep(&left, &left) == -1 && errno == EINTR) {}
} /* sleep_ms */

static void put(uint16_t kind, uint16_t what, int32_t val) {
    ev_t e;
    memset(&e, 0, sizeof e);
    e.kind = kind; e.what = what; e.val = val;
    ssize_t n = write(dev, &e, sizeof e);
    if (n != (ssize_t)sizeof e) fprintf(stderr, "touchsim: write failed: %s\n", strerror(errno));
} /* put */

static void report(void) { put(EVK_SYN, SYN_REPORT_CODE, 0); }
static void position(int x, int y) { put(EVK_ABS, ABS_MT_X_CODE, x); put(EVK_ABS, ABS_MT_Y_CODE, y); }

/* contact starts in slot 0 at (x, y): one complete frame */
static void press(int x, int y) {
    put(EVK_ABS, ABS_MT_TRACK_CODE, 0);
    position(x, y);
    put(EVK_KEY, BTN_TOUCH_CODE, 1);
    report();
} /* press */

/* contact ends where it was; no coordinates are sent */
static void lift(void) {
    put(EVK_KEY, BTN_TOUCH_CODE, 0);
    put(EVK_ABS, ABS_MT_TRACK_CODE, -1);
    report();
} /* lift */

/* Does this event node report both multi-touch position axes? */
static int is_touch_node(const char *p) {
    int fd = open(p, O_RDONLY | O_NONBLOCK), yes = 0;
    if (fd < 0) return 0;
    unsigned char bits[16] = { 0 }; /* one bit per ABS code 0..127 */
    /* EVIOCGBIT(EV_ABS, sizeof bits), built by hand: dir=read, size, type 'E', nr 0x20+EV_ABS */
    unsigned long rq = (2UL << 30) | ((unsigned long)sizeof bits << 16) | ('E' << 8) | (0x20 + EVK_ABS);
#define HAS(c) ((bits[(c) / 8] >> ((c) % 8)) & 1)
    if (ioctl(fd, rq, bits) >= 0) yes = HAS(ABS_MT_X_CODE) && HAS(ABS_MT_Y_CODE);
#undef HAS
    (void)close(fd);
    return yes;
} /* is_touch_node */

/* Writes the first touch-capable /dev/input/event* into out; false if none. */
static int pick_auto(char *out, size_t cap) {
    DIR *d = opendir("/dev/input");
    struct dirent *de;
    int ok = 0;
    if (d == NULL) return 0;
    while (!ok && (de = readdir(d)) != NULL) {
        char p[PATH_CAP];
        if (strncmp(de->d_name, "event", 5)) continue;
        snprintf(p, sizeof p, "/dev/input/%s", de->d_name);
        if (is_touch_node(p)) { snprintf(out, cap, "%s", p); ok = 1; }
    }
    closedir(d);
    return ok;
} /* pick_auto */

/* argv[3...] as integers into vals: at least `need`, at most `cap` read (words past
 * that are ignored), slots not given keep what the caller preset. False if bad. */
static int read_ints(int argc, char **argv, int need, int cap, int *vals) {
    int given = argc - 3;
    if (given < need) return 0;
    for (int i = 0; i < given && i < cap; i++) {
        char *rest;
        errno = 0;
        long n = strtol(argv[3 + i], &rest, 10);
        if (errno != 0 || rest == argv[3 + i] || *rest != '\0' || n < -100000 || n > 100000) return 0;
        vals[i] = (int)n;
    } /* for */
    return 1; /* all given values parsed */
} /* read_ints */

static int do_tap(const char *node, int argc, char **argv) {
    int xy[2];
    if (!read_ints(argc, argv, 2, 2, xy)) return 0;
    printf("tap %d,%d on %s\n", xy[0], xy[1], node);
    press(xy[0], xy[1]);
    sleep_ms(TAP_DOWN_MS);
    lift();
    return 1; /* tapped */
} /* do_tap */

static int do_swipe(const char *node, int argc, char **argv) {
    int a[5] = { 0, 0, 0, 0, 300 }; /* x0 y0 x1 y1 duration */
    if (!read_ints(argc, argv, 4, 5, a) || a[4] < 0) return 0;
    printf("swipe %d,%d -> %d,%d (%dms) on %s\n", a[0], a[1], a[2], a[3], a[4], node);
    int n = a[4] / STEP_MS;
    if (n < STEPS_AT_LEAST) n = STEPS_AT_LEAST;
    press(a[0], a[1]);
    for (int i = 1; i <= n; i++) {
        sleep_ms((long)a[4] / n);
        position(a[0] + (a[2] - a[0]) * i / n, a[1] + (a[3] - a[1]) * i / n);
        report();
    }
    sleep_ms(SWIPE_REST_MS);
    lift();
    return 1; /* done */
} /* do_swipe */

/* Press and release one key. Only the power key wakes the panel in the UI, so a
 * headless run that begins with the screen off sends this first. */
static int do_key(const char *node, int argc, char **argv) {
    int k[2] = { 0, 60 }; /* code, hold */
    if (!read_ints(argc, argv, 1, 2, k) || k[0] < 0 || k[0] > 0x2ff || k[1] < 0) return 0;
    printf("key %d (%dms) on %s\n", k[0], k[1], node);
    put(EVK_KEY, (uint16_t)k[0], 1); report();
    sleep_ms(k[1]);
    put(EVK_KEY, (uint16_t)k[0], 0); report();
    return 1; /* done */
} /* do_key */

typedef int (*action_fn)(const char *, int, char **);
static const struct action { const char *word; action_fn fn; const char *help; } ACTIONS[] = {
    { "tap", do_tap, "X Y" },
    { "swipe", do_swipe, "X0 Y0 X1 Y1 [MS]" },
    { "key", do_key, "CODE [HOLD_MS]   (KEY_POWER = 116, usually on /dev/input/event0)" },
};

#define ACTION_COUNT (sizeof ACTIONS / sizeof *ACTIONS)
static int print_usage(void) {
    for (size_t i = 0; i < ACTION_COUNT; i++)
        fprintf(stderr, "%s touchsim NODE|auto %s %s\n", i == 0 ? "usage:" : "      ", ACTIONS[i].word, ACTIONS[i].help);
    return 1; /* exit status */
} /* print_usage */

int main(int argc, char **argv) {
    const struct action *act = NULL;
    char node[PATH_CAP];
    if (argc < 3) return print_usage();
    for (size_t i = 0; i < ACTION_COUNT; i++)
        if (!strcmp(argv[2], ACTIONS[i].word)) act = &ACTIONS[i];
    if (act == NULL) {
        fprintf(stderr, "touchsim: unknown action '%s'\n", argv[2]);
        return print_usage();
    } /* unknown action */
    if (strcmp(argv[1], "auto") != 0) snprintf(node, sizeof node, "%s", argv[1]);
    else if (!pick_auto(node, sizeof node)) {
        fprintf(stderr, "touchsim: no multi-touch input node under /dev/input\n");
        return 1; /* nothing to write to */
    } /* auto */
    if ((dev = open(node, O_WRONLY)) < 0) {
        fprintf(stderr, "touchsim: cannot open %s: %s\n", node, strerror(errno));
        return 1;
    }
    int good = act->fn(node, argc, argv);
    close(dev);
    if (good) return 0;
    fprintf(stderr, "touchsim: bad arguments for '%s'\n", act->word);
    return print_usage();
} /* main */
