/*
 * qmi_nas_sub_probe - READ-ONLY: per-subscription signal on U60 (SDX75) over AF_QIPCRTR.
 * NAS (svc 0x03): Bind Subscription 0x0045 (this client only), Get Signal Info 0x004F,
 * Get Serving System 0x0024. Usage: qmi_nas_sub_probe <sub 0|1|-1 no bind> [wide]
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
#include <sys/socket.h>
#include <sys/time.h>

#ifndef AF_QIPCRTR
#define AF_QIPCRTR 42
#endif

struct sockaddr_qrtr {
    unsigned short sq_family;
    uint32_t       sq_node;
    uint32_t       sq_port;
};

#define QRTR_PORT_CTRL       0xfffffffeu
#define QRTR_TYPE_NEW_SERVER 4
#define QRTR_TYPE_NEW_LOOKUP 10

struct qrtr_ctrl_pkt {
    uint32_t cmd;
    struct { uint32_t service, instance, node, port; } server;
} __attribute__((packed));


#define SVC_NAS  0x03

static uint32_t g_node, g_port;
static uint16_t g_txn = 1;
static int g_timeout = 4;

static void hexdump(const char *tag, const uint8_t *b, size_t n) {
    printf("%s (%zu):", tag, n);
    for (size_t i = 0; i < n; i++) printf(" %02x", b[i]);
    printf("\n");
}

static const uint8_t *tlv_find(const uint8_t *body, size_t len, uint8_t type, uint16_t *outlen) {
    size_t i = 0;
    while (i + 3 <= len) {
        uint8_t t = body[i];
        uint16_t l = body[i+1] | (body[i+2] << 8);
        if (i + 3 + l > len) return NULL;
        if (t == type) { *outlen = l; return body + i + 3; }
        i += 3 + l;
    }
    return NULL;
}

static int lookup(int s, uint32_t svc) {
    struct qrtr_ctrl_pkt pkt = {0};
    pkt.cmd = QRTR_TYPE_NEW_LOOKUP;
    pkt.server.service = svc;
    struct sockaddr_qrtr local; socklen_t sl = sizeof(local);
    struct sockaddr_qrtr dst = { .sq_family = AF_QIPCRTR, .sq_node = 0, .sq_port = QRTR_PORT_CTRL };
    if (getsockname(s, (struct sockaddr*)&local, &sl) == 0 && local.sq_port != 0)
        dst.sq_node = local.sq_node;
    if (sendto(s, &pkt, sizeof(pkt), 0, (struct sockaddr*)&dst, sizeof(dst)) < 0) {
        getsockname(s, (struct sockaddr*)&local, &sl);
        dst.sq_node = local.sq_node;
        if (sendto(s, &pkt, sizeof(pkt), 0, (struct sockaddr*)&dst, sizeof(dst)) < 0) {
            perror("sendto(lookup)"); return -1;
        }
    }
    struct timeval tv = { .tv_sec = 2 };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    int found = 0;
    for (;;) {
        struct qrtr_ctrl_pkt rp;
        struct sockaddr_qrtr from; socklen_t fl = sizeof(from);
        ssize_t n = recvfrom(s, &rp, sizeof(rp), 0, (struct sockaddr*)&from, &fl);
        if (n < 0) break;
        if (from.sq_port != QRTR_PORT_CTRL || (size_t)n < sizeof(rp)) continue;
        if (rp.cmd != QRTR_TYPE_NEW_SERVER) continue;
        if (rp.server.service == 0 && rp.server.node == 0 && rp.server.port == 0) break;
        printf("[lookup] svc=0x%x inst=0x%x node=%u port=%u\n",
               rp.server.service, rp.server.instance, rp.server.node, rp.server.port);
        if (!found) { g_node = rp.server.node; g_port = rp.server.port; found = 1; }
    }
    if (!found) { printf("[!] service 0x%x not on QRTR bus\n", svc); return -1; }
    return 0;
}

/* Send one request and wait for its response. Returns body length (TLVs after
 * the 7-byte header) or -1; body copied into out. */
static int request(int s, uint16_t msgid, const uint8_t *tlvs, size_t tlen,
                   uint8_t *out, size_t outsz) {
    uint8_t pkt[256];
    if (tlen + 7 > sizeof(pkt)) return -1;
    uint16_t txn = g_txn++;
    pkt[0] = 0x00;
    pkt[1] = txn & 0xff; pkt[2] = txn >> 8;
    pkt[3] = msgid & 0xff; pkt[4] = msgid >> 8;
    pkt[5] = tlen & 0xff; pkt[6] = tlen >> 8;
    if (tlen) memcpy(pkt + 7, tlvs, tlen);
    struct sockaddr_qrtr dst = { .sq_family = AF_QIPCRTR, .sq_node = g_node, .sq_port = g_port };
    if (sendto(s, pkt, tlen + 7, 0, (struct sockaddr*)&dst, sizeof(dst)) != (ssize_t)(tlen + 7)) {
        perror("sendto"); return -1;
    }
    struct timeval tv = { .tv_sec = g_timeout };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    uint8_t buf[4096];
    for (;;) {
        struct sockaddr_qrtr from = {0}; socklen_t fl = sizeof(from);
        ssize_t n = recvfrom(s, buf, sizeof(buf), 0, (struct sockaddr*)&from, &fl);
        if (n < 0) { printf("[!] msg 0x%04x: %s\n", msgid, errno == EAGAIN ? "timeout" : strerror(errno)); return -1; }
        if (from.sq_node != g_node || from.sq_port != g_port || n < 7) continue;
        uint16_t rid = buf[3] | (buf[4] << 8);
        uint16_t rtxn = buf[1] | (buf[2] << 8);
        if (buf[0] != 0x02 || rid != msgid || rtxn != txn) continue;
        size_t blen = (size_t)n - 7;
        if (blen > outsz) blen = outsz;
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
