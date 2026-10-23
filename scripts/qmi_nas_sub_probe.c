/*
 * qmi_nas_sub_probe - READ-ONLY: per-subscription signal on U60 (SDX75) over AF_QIPCRTR.
 * NAS (svc 0x03): Bind Subscription 0x0045 (this client only), Get Signal Info 0x004F,
 * Get Serving System 0x0024. Usage: qmi_nas_sub_probe <sub 0|1|-1 no bind> [wide]
 */
#include <errno.h>      // errno
#include <stdint.h>     // fixed-width ints
#include <stdio.h>      // printf
#include <stdlib.h>     // atoi
#include <string.h>     // memcpy, strerror
#include <unistd.h>     // close
#include <sys/socket.h> // socket, sendto, recvfrom
#include <sys/time.h>   // struct timeval
#if !defined(AF_QIPCRTR)
#  define AF_QIPCRTR 42 /* missing from older libc headers */
#endif // AF_QIPCRTR

/* sockaddr_qrtr, written out so no kernel header is needed */
typedef struct {
    unsigned short fam;
    uint32_t node, port;
} qrtr_sa_t;
/* QRTR name service: its control port and the two packet types used */
enum { CTRL_PORT = 0xfffffffe };

enum { CTRL_NEW_SERVER = 4, CTRL_NEW_LOOKUP = 10 };

typedef struct __attribute__((packed)) {
    uint32_t cmd;
    uint32_t service, instance, node, port;
} ctrl_pkt_t;
/* the one QMI service this probe talks to */
enum { SVC_NAS = 0x03 };

/* where the service from the last lookup() lives, and the next transaction id */
static uint32_t g_node, g_port;
static uint16_t g_txn = 1;
static int g_timeout = 4; /* seconds to wait for a response */
static uint16_t rd16(const uint8_t *at) { return (uint16_t)(at[0] | (at[1] << 8)); }
static void hexdump(const char *tag, const uint8_t *b, size_t n) {
    printf("%s (%zu):", tag, n);
    for (size_t k = 0; k < n; k++)
        printf(" %02x", b[k]);
    putchar('\n');
}

/* TLV `type` within a response body (the bytes after the QMI header), or NULL. */
static const uint8_t *tlv_find(const uint8_t *body, size_t len, uint8_t type, uint16_t *outlen) {
    const uint8_t *hit = NULL;
    size_t pos = 0;
    while (hit == NULL && pos + 3 <= len) {
        const uint16_t size = rd16(body + pos + 1);
        if (pos + 3 + size > len)
            break; /* truncated: stop */
        if (body[pos] == type) {
            *outlen = size;
            hit = body + pos + 3;
        }
        pos += 3 + (size_t)size;
    }
    return hit;
}

/* NEW_LOOKUP for `svc`; prints every server listed and keeps the first. */
static int lookup(int s, uint32_t svc) {
    ctrl_pkt_t ask;
    memset(&ask, 0, sizeof ask);
    ask.cmd = CTRL_NEW_LOOKUP;
    ask.service = svc;
    qrtr_sa_t self;
    socklen_t self_len = sizeof self;
    qrtr_sa_t ctrl = { AF_QIPCRTR, 0, CTRL_PORT };
    if (getsockname(s, (struct sockaddr *)&self, &self_len) == 0 && self.port != 0)
        ctrl.node = self.node;
    int sent = (int)sendto(s, &ask, sizeof ask, 0, (struct sockaddr *)&ctrl, sizeof ctrl);
    if (sent < 0) {
        /* an unbound socket has no address until its first send: ask again, then retry */
        getsockname(s, (struct sockaddr *)&self, &self_len);
        ctrl.node = self.node;
        sent = (int)sendto(s, &ask, sizeof ask, 0, (struct sockaddr *)&ctrl, sizeof ctrl);
        if (sent < 0) {
            perror("sendto(lookup)");
            return -1;
        }
    }
    struct timeval limit = { 2, 0 };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &limit, sizeof limit);
    int have = 0;
    while (1) {
        ctrl_pkt_t ans;
        qrtr_sa_t src;
        socklen_t src_len = sizeof src;
        ssize_t got = recvfrom(s, &ans, sizeof ans, 0, (struct sockaddr *)&src, &src_len);
        if (got < 0)
            break; /* timeout ends the list */
        if (src.port != CTRL_PORT || (size_t)got < sizeof ans || ans.cmd != CTRL_NEW_SERVER)
            continue;
        if (ans.service == 0 && ans.node == 0 && ans.port == 0)
            break; /* all-zero record: end of list */
        printf("[lookup] svc=0x%x inst=0x%x node=%u port=%u\n", ans.service, ans.instance,
               ans.node, ans.port);
        if (!have) {
            g_node = ans.node;
            g_port = ans.port;
            have = 1;
        }
    }
    if (have)
        return 0;
    printf("[!] service 0x%x not on QRTR bus\n", svc);
    return -1;
}

/* One request/response. The body (TLVs after the 7-byte header) is copied into `out`,
 * cut at `outsz`; returns its length, or -1. */
static int request(int s, uint16_t msgid, const uint8_t *tlvs, size_t tlen,
                   uint8_t *out, size_t outsz) {
    uint8_t pkt[256];
    if (tlen + 7 > sizeof pkt)
        return -1;
    const uint16_t txn = g_txn++;
    const uint16_t hdr[3] = { txn, msgid, (uint16_t)tlen };
    pkt[0] = 0x00; /* request */
    for (int k = 0; k < 3; k++) {
        pkt[1 + 2 * k] = (uint8_t)hdr[k];
        pkt[2 + 2 * k] = (uint8_t)(hdr[k] >> 8);
    }
    if (tlen)
        memcpy(pkt + 7, tlvs, tlen);
    const qrtr_sa_t dst = { AF_QIPCRTR, g_node, g_port };
    const ssize_t total = (ssize_t)(tlen + 7);
    if (sendto(s, pkt, (size_t)total, 0, (const struct sockaddr *)&dst, sizeof dst) != total) {
        perror("sendto");
        return -1;
    }
    struct timeval limit = { g_timeout, 0 };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &limit, sizeof limit);
    uint8_t buf[4096];
    while (1) {
        qrtr_sa_t src = { 0 };
        socklen_t src_len = sizeof src;
        ssize_t n = recvfrom(s, buf, sizeof buf, 0, (struct sockaddr *)&src, &src_len);
        if (n < 0) {
            printf("[!] msg 0x%04x: %s\n", msgid, errno == EAGAIN ? "timeout" : strerror(errno));
            return -1;
        }
        if (src.node != g_node || src.port != g_port || n < 7)
            continue;
        /* a response (flag 0x02) to this very message and transaction */
        if (buf[0] != 0x02 || rd16(buf + 3) != msgid || rd16(buf + 1) != txn)
            continue;
        size_t blen = (size_t)n - 7;
        if (blen > outsz)
            blen = outsz;
        memcpy(out, buf + 7, blen);
        return (int)blen;
    }
}

static int show_result(const char *name, const uint8_t *b, int n) {
    uint16_t l = 0;
    const uint8_t *v = tlv_find(b, n, 0x02, &l);
    if (!v || l < 4) { printf("%s: no result TLV\n", name); return -1; }
    uint16_t res = v[0] | (v[1] << 8), err = v[2] | (v[3] << 8);
    printf("%s: result=%s qmi_error=%u\n", name, res ? "FAILURE" : "SUCCESS", err);
    return res ? -1 : 0;
}

static int req_ok(const char *name, int s, uint16_t id, const uint8_t *t, size_t tl, uint8_t *out, size_t osz) {
    int n = request(s, id, t, tl, out, osz);
    if (n < 0) return -1;
    if (show_result(name, out, n) < 0) { hexdump("  raw", out, n); return -1; }
    return n;
}

static void sig(const char *tag, const uint8_t *b, int n) {
    uint16_t l = 0; const uint8_t *v;
    if ((v = tlv_find(b, n, 0x14, &l)) && l >= 6)
        printf("  %s lte rssi=%d rsrq=%d rsrp=%d snr=%.1f\n", tag, (int8_t)v[0], (int8_t)v[1],
               (int16_t)(v[2] | v[3] << 8), (int16_t)(v[4] | v[5] << 8) / 10.0);
    if ((v = tlv_find(b, n, 0x17, &l)) && l >= 4)
        printf("  %s nr5g rsrp=%d snr=%.1f\n", tag, (int16_t)(v[0] | v[1] << 8), (int16_t)(v[2] | v[3] << 8) / 10.0);
    if ((v = tlv_find(b, n, 0x18, &l)) && l >= 2)
        printf("  %s nr5g rsrq=%d\n", tag, (int16_t)(v[0] | v[1] << 8));
    hexdump("  raw", b, n > 96 ? 96 : n);
}

/* read-only: bind THIS client to a subscription, then Get Signal Info 0x004F + Get Serving System 0x0024 */
int main(int argc, char **argv) {
    int sub = argc > 1 ? atoi(argv[1]) : 1, wide = argc > 2;
    int s = socket(AF_QIPCRTR, SOCK_DGRAM, 0);
    if (s < 0) { perror("socket(AF_QIPCRTR)"); return 1; }
    if (lookup(s, SVC_NAS) < 0) return 1;
    uint8_t b[4096];
    if (sub >= 0) {
        uint8_t t8[4] = { 0x01, 1, 0, (uint8_t)sub };
        uint8_t t32[7] = { 0x01, 4, 0, (uint8_t)sub, 0, 0, 0 };
        if (req_ok("bind_subscription", s, 0x0045, wide ? t32 : t8, wide ? sizeof(t32) : sizeof(t8), b, sizeof(b)) < 0)
            return 1;
    }
    int n = req_ok("get_signal_info", s, 0x004F, NULL, 0, b, sizeof(b));
    if (n > 0) sig("sig", b, n);
    n = req_ok("get_serving_system", s, 0x0024, NULL, 0, b, sizeof(b));
    if (n > 0) { uint16_t l = 0; const uint8_t *v = tlv_find(b, n, 0x01, &l);
        if (v && l >= 2) printf("  reg_state=%u cs=%u\n", v[0], v[1]);
        if ((v = tlv_find(b, n, 0x12, &l)) && l >= 5) printf("  plmn=%u-%u %.*s\n", v[0] | v[1] << 8, v[2] | v[3] << 8, v[4], (const char *)v + 5); }
    return 0;
}
