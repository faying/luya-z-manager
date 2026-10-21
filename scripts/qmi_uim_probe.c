/*
 * qmi_uim_probe — small QMI UIM client for the eUICC on the U60 (SDX75).
 *
 * Talks to the modem's UIM service (QMI service 11) over an AF_QIPCRTR
 * datagram socket. luyaz-esim relies on three subcommands and their output:
 *
 *   open [slot]     open + close a logical channel to the ISD-R; stderr gets
 *                   "[open] channel=N", or "qmi_error=N" when refused
 *                   (68 = no free channel)
 *   close <chan>    close a channel a killed lpac left open (the modem keeps
 *                   it until someone does)
 *   simreset [slot] power the card down and up so the modem re-reads it after
 *                   a profile switch (the stack ignores the card's REFRESH)
 *
 * Diagnostics: lookup, status, apps, iccid, eid, provision.
 *
 * Build (static, aarch64): zig cc -target aarch64-linux-musl -static -Os \
 *   qmi_uim_probe.c -o qmi_uim_probe
 */

#include <ctype.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#ifndef AF_QIPCRTR
#define AF_QIPCRTR 42
#endif

struct qrtr_addr {
    unsigned short family;
    uint32_t node;
    uint32_t port;
};

/* QRTR name service */
#define NS_PORT        0xfffffffeu
#define NS_NEW_SERVER  4
#define NS_NEW_LOOKUP  10

struct ns_packet {
    uint32_t type;
    uint32_t service, instance, node, port;
} __attribute__((packed));

/* QMI UIM */
#define SVC_UIM            11
#define MSG_READ_TRANSP    0x0020
#define MSG_CARD_STATUS    0x002F
#define MSG_POWER_DOWN     0x0030
#define MSG_POWER_UP       0x0031
#define MSG_CHANGE_PROV    0x0038
#define MSG_SEND_APDU      0x003B
#define MSG_CLOSE_CHANNEL  0x003F
#define MSG_OPEN_CHANNEL   0x0042

#define QMI_HDR   7     /* flags(1) txn(2) msg(2) len(2) */
#define QMI_RESP  0x02
#define MAXMSG    8192

/* ISD-R application id (GSMA SGP.22) */
static const uint8_t isdr_aid[16] = {
    0xA0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0xFF,
    0xFF, 0xFF, 0xFF, 0x89, 0x00, 0x00, 0x01, 0x00,
};

static int sock = -1;
static struct qrtr_addr uim;     /* where the UIM service lives */
static uint16_t next_txn = 1;
static uint8_t slot = 1;

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | p[1] << 8); }
static void put16(uint8_t *p, uint16_t v) { p[0] = v & 0xff; p[1] = v >> 8; }

static void dump(const char *what, const uint8_t *p, size_t n)
{
    fprintf(stderr, "%s, %zu bytes:", what, n);
    for (size_t i = 0; i < n; i++)
        fprintf(stderr, "%s%02x", i % 32 ? " " : "\n  ", p[i]);
    fputc('\n', stderr);
}

/* ---- message building ---- */

struct msg {
    uint8_t b[MAXMSG];
    size_t n;
};

static void msg_start(struct msg *m, uint16_t id)
{
    memset(m->b, 0, QMI_HDR);
    put16(m->b + 1, next_txn++);
    put16(m->b + 3, id);
    m->n = QMI_HDR;
}

static void msg_tlv(struct msg *m, uint8_t type, const void *v, uint16_t len)
{
    m->b[m->n] = type;
    put16(m->b + m->n + 1, len);
    memcpy(m->b + m->n + 3, v, len);
    m->n += 3 + (size_t)len;
}

static void msg_u8(struct msg *m, uint8_t type, uint8_t v) { msg_tlv(m, type, &v, 1); }

/* ---- reply parsing ---- */

struct reply {
    uint8_t b[MAXMSG];
    size_t n;
    uint16_t qerr;
};

/* Value of TLV `type` in a reply body, or NULL. */
static const uint8_t *tlv(const struct reply *r, uint8_t type, uint16_t *len)
{
    size_t at = QMI_HDR;
    while (at + 3 <= r->n) {
        uint16_t l = le16(r->b + at + 1);
        if (at + 3 + l > r->n)
            break;
        if (r->b[at] == type) {
            *len = l;
            return r->b + at + 3;
        }
        at += 3 + (size_t)l;
    }
    return NULL;
}

/* ---- transport ---- */

static void set_timeout(int ms)
{
    struct timeval tv = { ms / 1000, (ms % 1000) * 1000 };
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
}

/* Ask the name service where UIM is; the first server answering wins. */
static int find_uim(int list)
{
    struct qrtr_addr me = { 0 }, ns = { AF_QIPCRTR, 0, NS_PORT };
    socklen_t ml = sizeof me;
    struct ns_packet q = { .type = NS_NEW_LOOKUP, .service = SVC_UIM };

    if (getsockname(sock, (struct sockaddr *)&me, &ml) == 0)
        ns.node = me.node;
    if (sendto(sock, &q, sizeof q, 0, (struct sockaddr *)&ns, sizeof ns) < 0) {
        perror("lookup");
        return -1;
    }

    set_timeout(2000);
    int found = 0;
    for (;;) {
        struct ns_packet a;
        struct qrtr_addr from;
        socklen_t fl = sizeof from;
        ssize_t n = recvfrom(sock, &a, sizeof a, 0, (struct sockaddr *)&from, &fl);
        if (n < 0)
            break;
        if (from.port != NS_PORT || n < (ssize_t)sizeof a || a.type != NS_NEW_SERVER)
            continue;
        if (!a.service && !a.node && !a.port)
            break;                              /* end of the list */
        if (list)
            fprintf(stderr, "[uim] svc=%u inst=0x%x node=%u port=%u\n",
                    a.service, a.instance, a.node, a.port);
        if (!found) {
            uim = (struct qrtr_addr){ AF_QIPCRTR, a.node, a.port };
            found = 1;
        }
    }
    if (!found) {
        fprintf(stderr, "[!] no UIM service on QRTR bus\n");
        return -1;
    }
    fprintf(stderr, "[uim] using node=%u port=%u\n", uim.node, uim.port);
    return 0;
}

/*
 * Send `m`, wait for its response and read the result TLV.
 * Returns 0 on success, 1 when the modem refused (r->qerr set), -1 when no
 * answer came. Indications and other services' traffic are skipped.
 */
static int request(struct msg *m, struct reply *r, int timeout_ms)
{
    uint16_t id = le16(m->b + 3);
    put16(m->b + 5, (uint16_t)(m->n - QMI_HDR));
    if (sendto(sock, m->b, m->n, 0, (struct sockaddr *)&uim, sizeof uim) != (ssize_t)m->n) {
        perror("send");
        return -1;
    }

    set_timeout(timeout_ms);
    for (;;) {
        struct qrtr_addr from = { 0 };
        socklen_t fl = sizeof from;
        ssize_t n = recvfrom(sock, r->b, sizeof r->b, 0, (struct sockaddr *)&from, &fl);
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                fprintf(stderr, "recv: timeout\n");
            else
                perror("recv");
            return -1;
        }
        if (from.node != uim.node || from.port != uim.port || n < QMI_HDR)
            continue;
        if (r->b[0] == QMI_RESP && le16(r->b + 3) == id) {
            r->n = (size_t)n;
            break;
        }
    }

    uint16_t len = 0;
    const uint8_t *res = tlv(r, 0x02, &len);
    if (!res || len < 4) {
        fprintf(stderr, "[!] no result TLV\n");
        r->qerr = 0xffff;
        return 1;
    }
    r->qerr = le16(res + 2);
    if (le16(res) != 0) {
        fprintf(stderr, "[result] FAILURE qmi_error=%u\n", r->qerr);
        return 1;
    }
    return 0;
}

/* ---- logical channels ---- */

static int channel_open(void)
{
    struct msg m;
    struct reply r;
    uint8_t aid[1 + sizeof isdr_aid] = { sizeof isdr_aid };
    memcpy(aid + 1, isdr_aid, sizeof isdr_aid);

    msg_start(&m, MSG_OPEN_CHANNEL);
    msg_u8(&m, 0x01, slot);
    msg_tlv(&m, 0x10, aid, sizeof aid);
    int rc = request(&m, &r, 5000);
    if (rc < 0)
        return -1;
    if (rc > 0) {
        fprintf(stderr, "[!] open channel rejected by modem (qmi_error=%u)\n", r.qerr);
        return -1;
    }

    uint16_t len = 0;
    const uint8_t *ch = tlv(&r, 0x10, &len);
    if (!ch || len < 1) {
        fprintf(stderr, "[!] no channel id in response\n");
        return -1;
    }
    const uint8_t *sw = tlv(&r, 0x11, &len);
    if (sw && len >= 2)
        fprintf(stderr, "[open] channel=%u SW=%02X%02X\n", ch[0], sw[0], sw[1]);
    else
        fprintf(stderr, "[open] channel=%u\n", ch[0]);
    return ch[0];
}

static void channel_close(int ch)
{
    struct msg m;
    struct reply r;
    msg_start(&m, MSG_CLOSE_CHANNEL);
    msg_u8(&m, 0x01, slot);
    msg_u8(&m, 0x11, (uint8_t)ch);
    request(&m, &r, 3000);
    fprintf(stderr, "[close] channel %d released\n", ch);
}

/* One APDU on channel `ch`; the card's answer (data + SW) goes to `out`.
 * Returns its length, or -1. */
static int apdu(int ch, const uint8_t *cmd, uint16_t clen, uint8_t *out, size_t cap)
{
    struct msg m;
    struct reply r;
    uint8_t body[2 + 512];

    if (clen > sizeof body - 2)
        return -1;
    put16(body, clen);
    memcpy(body + 2, cmd, clen);
    msg_start(&m, MSG_SEND_APDU);
    msg_u8(&m, 0x01, slot);
    msg_tlv(&m, 0x02, body, (uint16_t)(2 + clen));
    msg_u8(&m, 0x10, (uint8_t)ch);

    dump("> apdu", cmd, clen);
    int rc = request(&m, &r, 5000);
    if (rc) {
        if (rc > 0)
            fprintf(stderr, "[!] send apdu rejected (qmi_error=%u)\n", r.qerr);
        return -1;
    }
    uint16_t len = 0;
    const uint8_t *v = tlv(&r, 0x10, &len);
    if (!v || len < 2 || le16(v) > len - 2 || le16(v) > cap) {
        fprintf(stderr, "[!] bad APDU response\n");
        return -1;
    }
    memcpy(out, v + 2, le16(v));
    dump("< apdu", out, le16(v));
    return le16(v);
}

/* ---- subcommands ---- */

static int do_open(void)
{
    int ch = channel_open();
    if (ch < 0)
        return 1;
    channel_close(ch);
    return 0;
}

static int do_close(const char *arg)
{
    int ch = arg ? atoi(arg) : 0;
    if (ch < 1 || ch > 19) {
        fprintf(stderr, "close needs a channel 1-19\n");
        return 1;
    }
    channel_close(ch);
    return 0;
}

static int do_status(void)
{
    struct msg m;
    struct reply r;
    msg_start(&m, MSG_CARD_STATUS);
    int rc = request(&m, &r, 3000);
    if (rc >= 0 || r.n)
        dump("< card status", r.b, r.n);
    return rc != 0;
}

/* Power-cycle: down, short pause, up. Up is sent even if down was refused. */
static int do_simreset(void)
{
    static const struct { uint16_t id; const char *name; } step[2] = {
        { MSG_POWER_DOWN, "power_down" },
        { MSG_POWER_UP, "power_up" },
    };
    for (int i = 0; i < 2; i++) {
        struct msg m;
        struct reply r;
        msg_start(&m, step[i].id);
        msg_u8(&m, 0x01, slot);
        int rc = request(&m, &r, 5000);
        if (rc < 0)
            return 1;
        fprintf(stderr, "[%s] %s (qmi_error=%u)\n", step[i].name,
                rc ? "FAILED" : "OK", rc ? r.qerr : 0);
        if (i == 0)
            usleep(500000);
    }
    return 0;
}

/*
 * Walk the card status TLV (0x10), print every app; return the first USIM
 * AID on slot 1 in `aid` (`*aid_len` stays 0 when there is none).
 * Layout: 4 x u16 session indexes, u8 slot count, then per slot
 * card_state, upin_state, upin_retries, upuk_retries, error, app count, and
 * per app type, state, perso_state, perso_feature, perso_retries,
 * perso_unblock_retries, aid_len, aid, then seven PIN bytes.
 */
static int do_apps(uint8_t *aid, uint8_t *aid_len)
{
    struct msg m;
    struct reply r;
    msg_start(&m, MSG_CARD_STATUS);
    if (request(&m, &r, 5000))
        return 1;

    uint16_t len = 0;
    const uint8_t *v = tlv(&r, 0x10, &len);
    if (!v || len < 9) {
        fprintf(stderr, "[!] no card status TLV\n");
        return 1;
    }
    uint16_t gw = le16(v);
    size_t at = 9;
    fprintf(stderr, "[cardstatus] gw_pri_index=0x%04x slots=%u\n", gw, v[8]);
    for (unsigned s = 0; s < v[8] && at + 6 <= len; s++) {
        unsigned napps = v[at + 5];
        fprintf(stderr, "  slot%u: card_state=%u err=%u apps=%u\n",
                s + 1, v[at], v[at + 4], napps);
        at += 6;
        for (unsigned a = 0; a < napps && at + 7 <= len; a++) {
            const uint8_t *app = v + at;
            uint8_t alen = app[6];
            if (at + 7 + alen + 7 > len)
                break;
            fprintf(stderr, "    app%u: type=%u state=%u aid=", a, app[0], app[1]);
            for (unsigned i = 0; i < alen; i++)
                fprintf(stderr, "%02x", app[7 + i]);
            fprintf(stderr, "%s\n", gw >> 8 == s && (gw & 0xff) == a
                                        ? "  <- GW provisioning app" : "");
            if (aid && s == 0 && app[0] == 2 && *aid_len == 0 && alen <= 32) {
                memcpy(aid, app + 7, alen);
                *aid_len = alen;
            }
            at += 7 + (size_t)alen + 7;
        }
    }
    return 0;
}

/* EF_ICCID (MF 3F00, file 2FE2) through one session type:
 * 6 = slot 1 card session, 0 = primary GW provisioning session. */
static int read_iccid(uint8_t session)
{
    static const uint8_t file[5] = { 0xE2, 0x2F, 0x02, 0x00, 0x3F };
    static const uint8_t whole[4] = { 0 };
    uint8_t sess[2] = { session, 0 };
    struct msg m;
    struct reply r;

    msg_start(&m, MSG_READ_TRANSP);
    msg_tlv(&m, 0x01, sess, 2);
    msg_tlv(&m, 0x02, file, sizeof file);
    msg_tlv(&m, 0x03, whole, sizeof whole);
    int rc = request(&m, &r, 5000);
    if (rc) {
        if (rc > 0)
            fprintf(stderr, "[iccid sess=%u] failed qmi_error=%u\n", session, r.qerr);
        return 1;
    }
    uint16_t len = 0;
    const uint8_t *v = tlv(&r, 0x11, &len);
    if (!v || len < 2) {
        fprintf(stderr, "[!] no read result\n");
        return 1;
    }
    /* BCD with swapped nibbles; an F nibble is padding */
    printf("iccid(sess=%u)=", session);
    for (unsigned i = 0; i < le16(v) && i < 10 && 2 + i < len; i++) {
        uint8_t b = v[2 + i];
        putchar('0' + (b & 0x0f));
        if ((b >> 4) <= 9)
            putchar('0' + (b >> 4));
    }
    putchar('\n');
    return 0;
}

static int do_iccid(void)
{
    int card = read_iccid(6);
    int prov = read_iccid(0);
    return card || prov;
}

/* ES10c GetEID: STORE DATA {BF3E 5C 5A}; collect 61xx continuations. */
static int do_eid(void)
{
    int ch = channel_open();
    if (ch < 0)
        return 1;

    uint8_t cla = 0x80 | (ch & 0x03);
    uint8_t cmd[16] = { cla, 0xE2, 0x91, 0x00, 0x06,
                        0xBF, 0x3E, 0x03, 0x5C, 0x01, 0x5A, 0x00 };
    uint16_t clen = 12;
    uint8_t all[1024], part[512];
    size_t got = 0;
    int done = 0;

    for (int i = 0; i < 8 && !done; i++) {
        int n = apdu(ch, cmd, clen, part, sizeof part);
        if (n < 2)
            break;
        uint8_t sw1 = part[n - 2], sw2 = part[n - 1];
        if (n > 2 && got + (size_t)(n - 2) <= sizeof all) {
            memcpy(all + got, part, (size_t)(n - 2));
            got += (size_t)(n - 2);
        }
        if (sw1 == 0x90 && sw2 == 0x00) {
            done = 1;
        } else if (sw1 == 0x61) {           /* GET RESPONSE for the rest */
            uint8_t more[5] = { cla, 0xC0, 0x00, 0x00, sw2 };
            memcpy(cmd, more, sizeof more);
            clen = sizeof more;
        } else {
            fprintf(stderr, "[!] unexpected SW=%02X%02X\n", sw1, sw2);
            break;
        }
    }
    channel_close(ch);

    if (!done) {
        fprintf(stderr, "[!] GetEID failed\n");
        return 1;
    }
    dump("[es10c] GetEID response", all, got);
    for (size_t i = 0; i + 18 <= got; i++) {
        if (all[i] == 0x5A && all[i + 1] == 0x10) {
            printf("EID=");
            for (int j = 0; j < 16; j++)
                printf("%02X", all[i + 2 + j]);
            printf("\n");
            return 0;
        }
    }
    fprintf(stderr, "[!] EID tag not found in response\n");
    return 1;
}

/* Point the primary GW session at slot 1's USIM again: deactivate, then
 * activate with the AID from the card status. */
static int do_provision(void)
{
    uint8_t aid[32];
    uint8_t aid_len = 0;
    if (do_apps(aid, &aid_len))
        return 1;
    if (!aid_len) {
        fprintf(stderr, "[!] no USIM app found on slot 1\n");
        return 1;
    }

    for (uint8_t activate = 0; activate < 2; activate++) {
        uint8_t sess[2] = { 0, activate };
        struct msg m;
        struct reply r;
        msg_start(&m, MSG_CHANGE_PROV);
        msg_tlv(&m, 0x01, sess, 2);
        if (activate) {
            uint8_t app[2 + 32] = { slot, aid_len };
            memcpy(app + 2, aid, aid_len);
            msg_tlv(&m, 0x10, app, (uint16_t)(2 + aid_len));
        }
        int rc = request(&m, &r, 5000);
        if (rc < 0)
            return 1;
        fprintf(stderr, "[prov %s] %s (qmi_error=%u)\n",
                activate ? "activate" : "deactivate", rc ? "FAILED" : "OK", rc ? r.qerr : 0);
        if (!activate)
            usleep(300000);
    }
    return 0;
}

int main(int argc, char **argv)
{
    const char *cmd = argc > 1 ? argv[1] : "";
    const char *arg = argc > 2 ? argv[2] : NULL;

    if (!*cmd) {
        fprintf(stderr, "usage: %s lookup|status|apps|iccid|eid|open|close <chan>|simreset|provision [slot]\n",
                argv[0]);
        return 2;
    }
    /* the optional trailing slot (1-8, default 1); close takes a channel there */
    if (arg && strcmp(cmd, "close") != 0) {
        int s = atoi(arg);
        if (s >= 1 && s <= 8)
            slot = (uint8_t)s;
    }

    sock = socket(AF_QIPCRTR, SOCK_DGRAM, 0);
    if (sock < 0) {
        perror("socket(AF_QIPCRTR)");
        return 1;
    }
    if (find_uim(!strcmp(cmd, "lookup")) < 0)
        return 1;

    int rc;
    if (!strcmp(cmd, "lookup"))         rc = 0;
    else if (!strcmp(cmd, "open"))      rc = do_open();
    else if (!strcmp(cmd, "close"))     rc = do_close(arg);
    else if (!strcmp(cmd, "simreset"))  rc = do_simreset();
    else if (!strcmp(cmd, "status"))    rc = do_status();
    else if (!strcmp(cmd, "apps"))      rc = do_apps(NULL, NULL);
    else if (!strcmp(cmd, "iccid"))     rc = do_iccid();
    else if (!strcmp(cmd, "eid"))       rc = do_eid();
    else if (!strcmp(cmd, "provision")) rc = do_provision();
    else {
        fprintf(stderr, "unknown command: %s\n", cmd);
        rc = 1;
    }
    close(sock);
    return rc;
}
