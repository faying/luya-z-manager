// qmi_uim_probe: a minimal QMI UIM (service 11) client for the removable
// eUICC of the U60, spoken over an AF_QIPCRTR datagram socket on the SDX75.
//
// Contract with luyaz-esim (it reads stderr; keep these lines as they are):
//   open [slot]      opens one logical channel to the ISD-R and closes it
//                    again. Prints "[open] channel=N" on success; a refusal
//                    prints "qmi_error=N" (68 means every channel is taken).
//   close <chan>     frees a channel that a killed lpac left behind; the
//                    modem never reclaims one on its own.
//   simreset [slot]  powers the card off and on, which makes the modem read
//                    it again after a profile switch (the stock stack does
//                    not react to the card's own REFRESH).
// Extra commands for poking around: lookup, status, apps, iccid, eid,
// provision.
//
// Built static for aarch64 by scripts/esim/build-esim-bundle.sh:
//   zig cc -target aarch64-linux-musl -static -Os qmi_uim_probe.c -o qmi_uim_probe
#include <errno.h> // errno
#include <stdint.h> // fixed-width ints
#include <stdio.h> // printf, fprintf
#include <stdlib.h> // atoi
#include <string.h> // memcpy, strcmp
#include <unistd.h> // close, usleep
#include <sys/socket.h> // socket, sendto, recvfrom
#include <sys/time.h> // struct timeval
#if !defined(AF_QIPCRTR)
#  define AF_QIPCRTR 42 /* not in older libc headers */
#endif // AF_QIPCRTR

/* ===== QRTR transport ===================================================== */
/* struct sockaddr_qrtr, spelled out so no kernel header is needed */
typedef struct {
    unsigned short fam;
    uint32_t node, port;
} qrtr_sa_t;
/* Control port of the QRTR name service, and the two packet types used. */
enum { CTRL_PORT = 0xfffffffe };

enum { CTRL_NEW_SERVER = 4, CTRL_NEW_LOOKUP = 10 };

typedef struct __attribute__((packed)) {
    uint32_t cmd;
    uint32_t service, instance, node, port;
} ctrl_pkt_t;

/* ===== QMI UIM vocabulary ================================================= */
enum { UIM_SERVICE = 11 };

/* request/response message ids */
enum {
    UIM_READ_TRANSPARENT    = 0x0020,
    UIM_GET_CARD_STATUS     = 0x002F,
    UIM_POWER_DOWN          = 0x0030,
    UIM_POWER_UP            = 0x0031,
    UIM_CHANGE_PROVISIONING = 0x0038,
    UIM_SEND_APDU           = 0x003B,
    UIM_LOGICAL_CHANNEL     = 0x003F, /* used here to close one */
    UIM_OPEN_LOGICAL_CHAN   = 0x0042,
};

/* Header: control flags (1), transaction (2), message id (2), TLV length (2). */
#define HDR_LEN        7
#define FLAG_RESPONSE  0x02
#define BUF_LEN        8192
#define FAIL           (-1) /* no answer / unusable */

/* GSMA SGP.22 ISD-R AID: A0000005591010FFFFFFFF8900000100 */
static const uint8_t ISDR[] = {
    0xA0, 0x00, 0x00, 0x05, 0x59, 0x10, 0x10, 0xFF,
    0xFF, 0xFF, 0xFF, 0x89, 0x00, 0x00, 0x01, 0x00,
};

/* ===== process state ====================================================== */
static struct {
    int fd;          /* the QRTR socket; it is also our QMI client identity */
    qrtr_sa_t peer;    /* UIM service address, found through the name service */
    uint16_t txn;    /* next transaction id */
    uint8_t slot;    /* physical slot (1-based) every slot TLV carries */
} g = { -1, { 0 }, 1, 1 };
static uint16_t rd16(const uint8_t *at)
{
    return (uint16_t)(at[0] | (at[1] << 8));
}

static void wr16(uint8_t *at, uint16_t val)
{
    at[0] = (uint8_t)val;
    at[1] = (uint8_t)(val >> 8);
}

/* Hex listing to stderr, 32 bytes per row. */
static void hexlog(const char *label, const uint8_t *data, size_t count)
{
    fprintf(stderr, "%s, %zu bytes:", label, count);
    for (size_t k = 0; k < count; k++) {
        const char *sep = (k % 32 == 0) ? "\n  " : " ";
        fprintf(stderr, "%s%02x", sep, data[k]);
    }
    fprintf(stderr, "\n");
}

static void rx_timeout(int millis)
{
    struct timeval limit;
    limit.tv_sec = millis / 1000;
    limit.tv_usec = (millis % 1000) * 1000;
    setsockopt(g.fd, SOL_SOCKET, SO_RCVTIMEO, &limit, sizeof limit);
}

/* ===== QMI frames ========================================================= */
typedef struct {
    uint8_t data[BUF_LEN];
    size_t used;
    uint16_t err; /* QMI error code of a refused request */
} frame_t;
/* Start a request: zeroed header with a fresh transaction and the message id. */
static void frame_begin(frame_t *f, uint16_t msg_id)
{
    memset(f->data, 0, HDR_LEN);
    wr16(&f->data[1], g.txn++);
    wr16(&f->data[3], msg_id);
    f->used = HDR_LEN;
}

static void frame_add(frame_t *f, uint8_t type, const void *val, uint16_t vlen)
{
    uint8_t *at = f->data + f->used;
    at[0] = type;
    wr16(at + 1, vlen);
    memcpy(at + 3, val, vlen);
    f->used += (size_t)vlen + 3;
}

static void frame_add_byte(frame_t *f, uint8_t type, uint8_t val)
{
    frame_add(f, type, &val, 1);
}

/* Find TLV `type` in a received frame; NULL if absent or truncated. */
static const uint8_t *frame_get(const frame_t *f, uint8_t type, uint16_t *vlen)
{
    const uint8_t *hit = NULL;
    size_t pos = HDR_LEN;
    while (hit == NULL && pos + 3 <= f->used) {
        const uint16_t size = rd16(f->data + pos + 1);
        if (pos + 3 + size > f->used)
            break; /* truncated TLV: stop looking */
        if (f->data[pos] == type) {
            *vlen = size;
            hit = f->data + pos + 3;
        }
        pos += (size_t)size + 3;
    }
    return hit;
}

/* ===== service discovery ================================================== */
/* NEW_LOOKUP for UIM; take the first server the name service reports.
 * With `verbose`, every server it lists is printed. */
static int locate_uim(int verbose)
{
    ctrl_pkt_t ask;
    qrtr_sa_t self = { 0 };
    qrtr_sa_t ctrl = { AF_QIPCRTR, 0, CTRL_PORT };
    socklen_t self_len = sizeof self;
    memset(&ask, 0, sizeof ask);
    ask.cmd = CTRL_NEW_LOOKUP;
    ask.service = UIM_SERVICE;
    if (getsockname(g.fd, (struct sockaddr *)&self, &self_len) == 0)
        ctrl.node = self.node;
    if (sendto(g.fd, &ask, sizeof ask, 0, (struct sockaddr *)&ctrl, sizeof ctrl) < 0) {
        perror("lookup");
        return FAIL;
    }
    int have = 0;
    rx_timeout(2000);
    while (1) {
        ctrl_pkt_t ans;
        qrtr_sa_t src;
        socklen_t src_len = sizeof src;
        ssize_t got = recvfrom(g.fd, &ans, sizeof ans, 0, (struct sockaddr *)&src, &src_len);
        if (got < 0)
            break; /* timed out: the list is over */
        int relevant = src.port == CTRL_PORT && got >= (ssize_t)sizeof ans
                       && ans.cmd == CTRL_NEW_SERVER;
        if (!relevant)
            continue;
        if (ans.service == 0 && ans.node == 0 && ans.port == 0)
            break; /* all-zero record terminates the list */
        if (verbose) /* "lookup" lists them all */
            fprintf(stderr, "[uim] svc=%u inst=0x%x node=%u port=%u\n", ans.service,
                    ans.instance, ans.node, ans.port);
        if (have)
            continue;
        g.peer = (qrtr_sa_t){ AF_QIPCRTR, ans.node, ans.port };
        have = 1;
    }
    if (have) {
        fprintf(stderr, "[uim] using node=%u port=%u\n", g.peer.node, g.peer.port);
        return EXIT_SUCCESS;
    }
    fprintf(stderr, "[!] no UIM service on QRTR bus\n");
    return FAIL;
}

/* ===== request / response ================================================= */
/*
 * Send `req` and wait up to `ms` for the response carrying the same message
 * id (indications and unrelated traffic are ignored). Result codes:
 *   0   the modem accepted it;
 *   1   the modem refused it, the QMI error is in rsp->err;
 *   -1  nothing usable came back.
 */
static int transact(frame_t *req, frame_t *rsp, int ms)
{
    const uint16_t want = rd16(req->data + 3);
    const ssize_t total = (ssize_t)req->used;
    wr16(req->data + 5, (uint16_t)(req->used - HDR_LEN));
    rsp->used = 0;
    if (sendto(g.fd, req->data, req->used, 0, (struct sockaddr *)&g.peer, sizeof g.peer) != total) {
        perror("send");
        return FAIL;
    }
    rx_timeout(ms);
    while (1) {
        qrtr_sa_t src = { 0 };
        socklen_t src_len = sizeof src;
        ssize_t got = recvfrom(g.fd, rsp->data, sizeof rsp->data, 0,
                               (struct sockaddr *)&src, &src_len);
        if (got < 0) {
            int timed_out = (errno == EAGAIN || errno == EWOULDBLOCK);
            if (timed_out)
                fprintf(stderr, "recv: timeout\n");
            else
                perror("recv");
            return FAIL;
        }
        int from_uim = src.node == g.peer.node && src.port == g.peer.port;
        if (!from_uim || got < HDR_LEN)
            continue;
        if (rsp->data[0] != FLAG_RESPONSE || rd16(rsp->data + 3) != want)
            continue;
        rsp->used = (size_t)got;
        break;
    }
    /* mandatory result TLV 0x02: u16 result, u16 error */
    uint16_t rlen = 0;
    const uint8_t *result = frame_get(rsp, 0x02, &rlen);
    if (result == NULL || rlen < 4) {
        fprintf(stderr, "[!] no result TLV\n");
        rsp->err = 0xffff;
        return EXIT_FAILURE;
    }
    rsp->err = rd16(result + 2);
    if (rd16(result) == 0)
        return EXIT_SUCCESS;
    fprintf(stderr, "[result] FAILURE qmi_error=%u\n", rsp->err);
    return EXIT_FAILURE;
}

/* ===== logical channels and APDUs ========================================= */
/* Returns the channel number, or -1. */
static int chan_open(void)
{
    frame_t req, rsp;
    uint8_t aid_tlv[sizeof ISDR + 1];
    aid_tlv[0] = sizeof ISDR;
    memcpy(&aid_tlv[1], ISDR, sizeof ISDR);
    frame_begin(&req, UIM_OPEN_LOGICAL_CHAN);
    frame_add_byte(&req, 0x01, g.slot);
    frame_add(&req, 0x10, aid_tlv, sizeof aid_tlv);
    switch (transact(&req, &rsp, 5000)) {
    case 0:
        break;
    case 1:
        fprintf(stderr, "[!] open channel rejected by modem (qmi_error=%u)\n", rsp.err);
        return FAIL;
    default:
        return FAIL;
    }
    uint16_t vlen = 0;
    const uint8_t *id = frame_get(&rsp, 0x10, &vlen);
    if (id == NULL || vlen == 0) {
        fprintf(stderr, "[!] no channel id in response\n");
        return FAIL;
    } /* TLV 0x10 = channel id, 0x11 = SW of the SELECT */
    unsigned chan = id[0];
    const uint8_t *status = frame_get(&rsp, 0x11, &vlen);
    if (status != NULL && vlen >= 2) {
        fprintf(stderr, "[open] channel=%u SW=%02X%02X\n", chan, status[0], status[1]);
    } else {
        fprintf(stderr, "[open] channel=%u\n", chan);
    }
    return (int)chan;
}

static void chan_close(int chan)
{
    frame_t req, rsp;
    frame_begin(&req, UIM_LOGICAL_CHANNEL);
    frame_add_byte(&req, 0x01, g.slot);
    frame_add_byte(&req, 0x11, (uint8_t)chan);
    (void)transact(&req, &rsp, 3000);
    fprintf(stderr, "[close] channel %d released\n", chan);
}

/* Exchange one APDU on `chan`. The answer (data then SW1 SW2) lands in `out`;
 * returns its length or -1. */
static int send_apdu(int chan, const uint8_t *capdu, uint16_t clen, uint8_t *out, size_t room)
{
    enum { MAX_CAPDU = 512 };
    uint8_t wrapped[2 + MAX_CAPDU];
    frame_t req, rsp;
    if (clen > MAX_CAPDU)
        return FAIL;
    wr16(wrapped, clen);
    memcpy(wrapped + 2, capdu, clen);
    frame_begin(&req, UIM_SEND_APDU);
    frame_add_byte(&req, 0x01, g.slot);
    frame_add(&req, 0x02, wrapped, (uint16_t)(clen + 2));
    frame_add_byte(&req, 0x10, (uint8_t)chan);
    hexlog("> apdu", capdu, clen);
    int rc = transact(&req, &rsp, 5000);
    if (rc == 1)
        fprintf(stderr, "[!] send apdu rejected (qmi_error=%u)\n", rsp.err);
    if (rc != 0)
        return FAIL;
    uint16_t vlen = 0;
    const uint8_t *rapdu = frame_get(&rsp, 0x10, &vlen);
    uint16_t size = (rapdu != NULL && vlen >= 2) ? rd16(rapdu) : 0;
    if (rapdu == NULL || vlen < 2 || size > vlen - 2 || size > room) {
        fprintf(stderr, "[!] bad APDU response\n");
        return FAIL;
    }
    memcpy(out, rapdu + 2, size);
    hexlog("< apdu", out, size);
    return size;
}

/* ===== commands =========================================================== */
static int cmd_open(void)
{
    int chan = chan_open();
    if (chan >= 0)
        chan_close(chan);
    return chan >= 0 ? 0 : 1;
}

static int cmd_close(const char *text)
{
    int chan = (text != NULL) ? atoi(text) : 0;
    if (chan >= 1 && chan <= 19) {
        chan_close(chan);
        return EXIT_SUCCESS;
    }
    fprintf(stderr, "close needs a channel 1-19\n");
    return EXIT_FAILURE;
}

static int cmd_status(void)
{
    frame_t req, rsp;
    frame_begin(&req, UIM_GET_CARD_STATUS);
    int rc = transact(&req, &rsp, 3000);
    if (rc >= 0 || rsp.used > 0)
        hexlog("< card status", rsp.data, rsp.used);
    return rc != 0;
}

/* Card off, half a second, card on. The power-up goes out even when the
 * power-down was refused. */
static int cmd_simreset(void)
{
    const uint16_t ids[2] = { UIM_POWER_DOWN, UIM_POWER_UP };
    const char *const labels[2] = { "power_down", "power_up" };
    for (int k = 0; k < 2; k++) {
        frame_t req, rsp;
        frame_begin(&req, ids[k]);
        frame_add_byte(&req, 0x01, g.slot);
        int rc = transact(&req, &rsp, 5000);
        if (rc < 0)
            return EXIT_FAILURE;
        fprintf(stderr, "[%s] %s (qmi_error=%u)\n", labels[k],
                rc == 0 ? "OK" : "FAILED", rc == 0 ? 0u : rsp.err);
        if (k == 0)
            usleep(500 * 1000);
    }
    return EXIT_SUCCESS;
}

/*
 * Print every application in the card status (TLV 0x10). When `usim` is
 * given, the first USIM (type 2) AID of slot 1 is copied there and its
 * length stored in `*usim_len` (left at 0 if there is none).
 *
 * TLV 0x10 layout:
 *   u16 x4 session indexes (the first is the GW primary: slot<<8 | app)
 *   u8     number of slots, then for each slot:
 *     u8 card state, u8 UPIN state, u8 UPIN retries, u8 UPUK retries,
 *     u8 error code, u8 number of apps, then for each app:
 *       u8 type, u8 state, u8 perso state, u8 perso feature,
 *       u8 perso retries, u8 perso unblock retries, u8 AID length,
 *       AID bytes, then 7 bytes of PIN data.
 */
static int cmd_apps(uint8_t *usim, uint8_t *usim_len)
{
    frame_t req, rsp;
    frame_begin(&req, UIM_GET_CARD_STATUS);
    if (transact(&req, &rsp, 5000) != 0)
        return EXIT_FAILURE;
    uint16_t vlen = 0;
    const uint8_t *st = frame_get(&rsp, 0x10, &vlen);
    if (st == NULL || vlen < 9) {
        fprintf(stderr, "[!] no card status TLV\n");
        return EXIT_FAILURE;
    }
    const uint16_t gw_index = rd16(st);
    const unsigned slots = st[8];
    fprintf(stderr, "[cardstatus] gw_pri_index=0x%04x slots=%u\n", gw_index, slots);
    size_t pos = 9;
    for (unsigned sl = 0; sl < slots; sl++) {
        if (pos + 6 > vlen)
            break;
        const uint8_t *card = st + pos;
        const unsigned apps = card[5];
        fprintf(stderr, "  slot%u: card_state=%u err=%u apps=%u\n", sl + 1, card[0], card[4], apps);
        pos += 6;
        for (unsigned ap = 0; ap < apps; ap++) {
            if (pos + 7 > vlen)
                break;
            const uint8_t *app = st + pos;
            const uint8_t aid_len = app[6];
            const size_t span = 7 + (size_t)aid_len + 7;
            if (pos + span > vlen)
                break;
            fprintf(stderr, "    app%u: type=%u state=%u aid=", ap, app[0], app[1]);
            for (unsigned k = 0; k < aid_len; k++)
                fprintf(stderr, "%02x", app[7 + k]);
            int is_gw = (gw_index >> 8) == sl && (gw_index & 0xff) == ap;
            fprintf(stderr, "%s\n", is_gw ? "  <- GW provisioning app" : "");
            int take = usim != NULL && sl == 0 && app[0] == 2 && *usim_len == 0 && aid_len <= 32;
            if (take) {
                memcpy(usim, app + 7, aid_len);
                *usim_len = aid_len;
            }
            pos += span;
        }
    }
    return EXIT_SUCCESS;
}

/* Read EF_ICCID (path 3F00/2FE2) through session type `session`
 * (6 = slot-1 card session, 0 = primary GW provisioning session). */
static int print_iccid(uint8_t session)
{
    static const uint8_t ef_path[5] = { 0xE2, 0x2F, 0x02, 0x00, 0x3F }; /* file id, path len, path */
    static const uint8_t read_all[4] = { 0, 0, 0, 0 };                 /* offset 0, length 0 */
    const uint8_t sess_tlv[2] = { session, 0 };
    frame_t req, rsp;
    frame_begin(&req, UIM_READ_TRANSPARENT);
    frame_add(&req, 0x01, sess_tlv, sizeof sess_tlv);
    frame_add(&req, 0x02, ef_path, sizeof ef_path);
    frame_add(&req, 0x03, read_all, sizeof read_all);
    int rc = transact(&req, &rsp, 5000);
    if (rc == 1)
        fprintf(stderr, "[iccid sess=%u] failed qmi_error=%u\n", session, rsp.err);
    if (rc != 0)
        return EXIT_FAILURE;
    uint16_t vlen = 0;
    const uint8_t *content = frame_get(&rsp, 0x11, &vlen);
    if (content == NULL || vlen < 2) {
        fprintf(stderr, "[!] no read result\n");
        return EXIT_FAILURE;
    }
    /* swapped-nibble BCD; a nibble above 9 is filler */
    printf("iccid(sess=%u)=", session);
    const unsigned size = rd16(content);
    for (unsigned k = 0; k < 10 && k < size && k + 2 < vlen; k++) {
        const uint8_t pair = content[2 + k];
        putchar('0' + (pair & 0x0f));
        if ((pair >> 4) < 10)
            putchar('0' + (pair >> 4));
    }
    putchar('\n');
    return EXIT_SUCCESS;
}

static int cmd_iccid(void)
{
    int bad_card = print_iccid(6);
    int bad_prov = print_iccid(0);
    return (bad_card || bad_prov) ? 1 : 0;
}

/* ES10c GetEID = STORE DATA carrying BF3E { 5C 01 5A }; 61xx means more bytes
 * wait behind a GET RESPONSE. The EID is the 16 bytes after tag 5A len 10. */
static int cmd_eid(void)
{
    int chan = chan_open();
    if (chan < 0)
        return EXIT_FAILURE;
    const uint8_t cla = (uint8_t)(0x80 | (chan & 0x03));
    uint8_t capdu[16] = { cla, 0xE2, 0x91, 0x00, 0x06, 0xBF, 0x3E, 0x03, 0x5C, 0x01, 0x5A, 0x00 };
    uint16_t capdu_len = 12;
    uint8_t joined[1024];
    uint8_t chunk[512];
    size_t joined_len = 0;
    int ok = 0;
    for (int round = 0; round < 8; round++) {
        int n = send_apdu(chan, capdu, capdu_len, chunk, sizeof chunk);
        if (n < 2)
            break;
        const size_t body = (size_t)n - 2;
        const uint8_t sw1 = chunk[body], sw2 = chunk[body + 1];
        if (body > 0 && joined_len + body <= sizeof joined) {
            memcpy(joined + joined_len, chunk, body);
            joined_len += body;
        }
        ok = (sw1 == 0x90 && sw2 == 0x00);
        if (ok)
            break; /* 9000: all of it is here */
        if (sw1 != 0x61) { /* neither done nor "more data" */
            fprintf(stderr, "[!] unexpected SW=%02X%02X\n", chunk[body], chunk[body + 1]);
            round = 8; /* give up */
            continue;
        }
        /* GET RESPONSE for the remaining sw2 bytes */
        const uint8_t get_response[5] = { cla, 0xC0, 0x00, 0x00, sw2 };
        memcpy(capdu, get_response, sizeof get_response);
        capdu_len = sizeof get_response;
    }
    chan_close(chan);
    if (!ok) {
        fprintf(stderr, "[!] GetEID failed\n");
        return EXIT_FAILURE;
    } /* now look for tag 5A, length 0x10 */
    hexlog("[es10c] GetEID response", joined, joined_len);
    for (size_t at = 0; at + 18 <= joined_len; at++) {
        if (joined[at] != 0x5A || joined[at + 1] != 0x10)
            continue;
        fputs("EID=", stdout);
        for (int k = 2; k < 18; k++)
            printf("%02X", joined[at + k]);
        putchar('\n');
        return EXIT_SUCCESS;
    }
    fputs("[!] EID tag not found in response\n", stderr);
    return EXIT_FAILURE;
}

/* Re-point the primary GW provisioning session at slot 1's USIM: deactivate
 * it, wait 300 ms, then activate it with the AID found in the card status. */
static int cmd_provision(void)
{
    uint8_t usim[32];
    uint8_t usim_len = 0;
    if (cmd_apps(usim, &usim_len) != 0)
        return EXIT_FAILURE;
    if (usim_len == 0) {
        fprintf(stderr, "[!] no USIM app found on slot 1\n");
        return EXIT_FAILURE;
    }
    static const char *const step_name[2] = { "deactivate", "activate" };
    for (uint8_t act = 0; act <= 1; act++) {
        const uint8_t sess_tlv[2] = { 0, act }; /* session 0 = primary GW */
        frame_t req, rsp;
        frame_begin(&req, UIM_CHANGE_PROVISIONING);
        frame_add(&req, 0x01, sess_tlv, sizeof sess_tlv);
        if (act) {
            uint8_t app_tlv[2 + 32];
            app_tlv[0] = g.slot;
            app_tlv[1] = usim_len;
            memcpy(app_tlv + 2, usim, usim_len);
            frame_add(&req, 0x10, app_tlv, (uint16_t)(usim_len + 2));
        }
        int rc = transact(&req, &rsp, 5000);
        if (rc < 0)
            return EXIT_FAILURE;
        fprintf(stderr, "[prov %s] %s (qmi_error=%u)\n", step_name[act],
                rc == 0 ? "OK" : "FAILED", rc == 0 ? 0u : rsp.err);
        if (act == 0)
            usleep(300 * 1000);
    }
    return EXIT_SUCCESS;
}

/* ===== entry ============================================================== */
static int is(const char *a, const char *b)
{
    return strcmp(a, b) == 0;
}

int main(int argc, char **argv)
{
    const char *verb = (argc >= 2) ? argv[1] : "";
    const char *extra = (argc >= 3) ? argv[2] : NULL;
    if (verb[0] == '\0') {
        fprintf(stderr, "usage: %s lookup|status|apps|iccid|eid|open|close <chan>|simreset|provision [slot]\n",
                argv[0]);
        return 2; /* usage error */
    }
    /* Second argument: the slot (1-8, default 1), except for close where it
     * is the channel. */
    if (extra != NULL && !is(verb, "close")) {
        int want = atoi(extra);
        if (want >= 1 && want <= 8)
            g.slot = (uint8_t)want;
    }
    g.fd = socket(AF_QIPCRTR, SOCK_DGRAM, 0);
    if (g.fd < 0) {
        perror("socket(AF_QIPCRTR)");
        return EXIT_FAILURE;
    }
    int rc = 1;
    if (locate_uim(is(verb, "lookup")) == 0) {
        if (is(verb, "lookup"))
            rc = 0;
        else if (is(verb, "open"))
            rc = cmd_open();
        else if (is(verb, "close"))
            rc = cmd_close(extra);
        else if (is(verb, "simreset"))
            rc = cmd_simreset();
        else if (is(verb, "status"))
            rc = cmd_status();
        else if (is(verb, "apps"))
            rc = cmd_apps(NULL, NULL);
        else if (is(verb, "iccid"))
            rc = cmd_iccid();
        else if (is(verb, "eid"))
            rc = cmd_eid();
        else if (is(verb, "provision"))
            rc = cmd_provision();
        else
            fprintf(stderr, "unknown command: %s\n", verb);
        close(g.fd);
    }
    return rc; /* 0 = done, 1 = failed */
}
