/*
 * qmi_uim_probe - Verify eUICC (removable eSIM) APDU access on U60 Pro (SDX75)
 *
 * Over AF_QIPCRTR: discover the QMI UIM service (svc id 11), open a logical
 * channel to the ISD-R applet, and read the eUICC's EID via ES10c GetEID.
 *
 * Usage:
 *   qmi_uim_probe lookup          - list UIM service instances on the QRTR bus
 *   qmi_uim_probe status          - UIM Get Card Status (raw hexdump)
 *   qmi_uim_probe open            - open logical channel to ISD-R, print SW
 *   qmi_uim_probe eid             - full check: open ISD-R -> GetEID -> close
 *
 * Cross-compile on macOS:
 *   zig cc -target aarch64-linux-musl -static qmi_uim_probe.c -o qmi_uim_probe
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
#include <ctype.h>
#include <sys/socket.h>
#include <sys/types.h>

#ifndef AF_QIPCRTR
#define AF_QIPCRTR 42
#endif

struct sockaddr_qrtr {
    unsigned short sq_family;
    uint32_t       sq_node;
    uint32_t       sq_port;
};

/* QRTR name-service control packets */
#define QRTR_PORT_CTRL       0xfffffffeu
#define QRTR_TYPE_NEW_SERVER 4
#define QRTR_TYPE_DEL_SERVER 5
#define QRTR_TYPE_NEW_LOOKUP 10

struct qrtr_ctrl_pkt {
    uint32_t cmd;
    struct { uint32_t service, instance, node, port; } server;
} __attribute__((packed));

/* QMI UIM service (id 11) message ids */
#define UIM_SVC_ID            11
#define UIM_READ_TRANSPARENT  0x0020
#define UIM_GET_CARD_STATUS   0x002F
#define UIM_POWER_DOWN        0x0030
#define UIM_POWER_UP          0x0031
#define UIM_CHANGE_PROV       0x0038
#define UIM_SEND_APDU         0x003B
#define UIM_LOGICAL_CHANNEL   0x003F
#define UIM_OPEN_LOGICAL_CHAN 0x0042

/* SGP.02/22 ISD-R AID */
static const uint8_t ISDR_AID[] = {
    0xA0,0x00,0x00,0x05,0x59,0x10,0x10,0xFF,0xFF,0xFF,0xFF,0x89,0x00,0x00,0x01,0x00
};

static uint8_t g_slot = 1;  /* target slot; override with a trailing arg */
#define BUF_SZ 8192

static void hexdump(const char *tag, const uint8_t *p, size_t n) {
    fprintf(stderr, "%s (%zu bytes):\n", tag, n);
    for (size_t i = 0; i < n; i += 16) {
        fprintf(stderr, "  %04zx: ", i);
        for (size_t j = 0; j < 16; j++) {
            if (i+j < n) fprintf(stderr, "%02x ", p[i+j]);
            else fprintf(stderr, "   ");
        }
        fprintf(stderr, " ");
        for (size_t j = 0; j < 16 && i+j < n; j++) {
            uint8_t c = p[i+j];
            fputc(isprint(c) ? c : '.', stderr);
        }
        fputc('\n', stderr);
    }
}

static size_t qmi_build_header(uint8_t *buf, uint16_t txn, uint16_t msgid, uint16_t msglen) {
    buf[0] = 0x00;
    buf[1] = txn & 0xff; buf[2] = txn >> 8;
    buf[3] = msgid & 0xff; buf[4] = msgid >> 8;
    buf[5] = msglen & 0xff; buf[6] = msglen >> 8;
    return 7;
}

static size_t tlv_add(uint8_t *p, uint8_t type, const void *val, uint16_t len) {
    p[0] = type;
    p[1] = len & 0xff; p[2] = len >> 8;
    memcpy(p + 3, val, len);
    return 3 + len;
}

static const uint8_t *tlv_find(const uint8_t *body, size_t body_len, uint8_t type, uint16_t *outlen) {
    size_t i = 0;
    while (i + 3 <= body_len) {
        uint8_t t = body[i];
        uint16_t l = body[i+1] | (body[i+2] << 8);
        if (i + 3 + l > body_len) return NULL;
        if (t == type) { *outlen = l; return body + i + 3; }
        i += 3 + l;
    }
    return NULL;
}

static uint32_t g_uim_node, g_uim_port;
static uint16_t g_txn = 1;

static int qrtr_open(void) {
    int s = socket(AF_QIPCRTR, SOCK_DGRAM, 0);
    if (s < 0) { perror("socket(AF_QIPCRTR)"); return -1; }
    return s;
}

/* Discover UIM service via QRTR name service. Returns 0 and fills
 * g_uim_node/g_uim_port with the first match. */
static int uim_lookup(int s, int verbose) {
    struct sockaddr_qrtr local;
    socklen_t sl = sizeof(local);
    /* Auto-bind so getsockname yields our node */
    struct qrtr_ctrl_pkt pkt = {0};
    pkt.cmd = QRTR_TYPE_NEW_LOOKUP;
    pkt.server.service = UIM_SVC_ID;
    pkt.server.instance = 0;

    struct sockaddr_qrtr dst = { .sq_family = AF_QIPCRTR, .sq_node = 0, .sq_port = QRTR_PORT_CTRL };
    /* learn local node first (bind implicitly) */
    if (getsockname(s, (struct sockaddr*)&local, &sl) == 0 && local.sq_port != 0)
        dst.sq_node = local.sq_node;
    if (sendto(s, &pkt, sizeof(pkt), 0, (struct sockaddr*)&dst, sizeof(dst)) < 0) {
        /* some kernels want the lookup sent to our own node's ctrl port after bind */
        getsockname(s, (struct sockaddr*)&local, &sl);
        dst.sq_node = local.sq_node;
        if (sendto(s, &pkt, sizeof(pkt), 0, (struct sockaddr*)&dst, sizeof(dst)) < 0) {
            perror("sendto(ctrl lookup)");
            return -1;
        }
    }

    struct timeval tv = { .tv_sec = 2, .tv_usec = 0 };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    int found = 0;
    for (;;) {
        struct qrtr_ctrl_pkt rp;
        struct sockaddr_qrtr from; socklen_t fl = sizeof(from);
        ssize_t n = recvfrom(s, &rp, sizeof(rp), 0, (struct sockaddr*)&from, &fl);
        if (n < 0) break; /* timeout = end of list on kernels without terminator */
        if (from.sq_port != QRTR_PORT_CTRL) continue;
        if ((size_t)n < sizeof(rp)) continue;
        if (rp.cmd != QRTR_TYPE_NEW_SERVER) continue;
        if (rp.server.service == 0 && rp.server.node == 0 && rp.server.port == 0)
            break; /* terminator */
        if (verbose)
            fprintf(stderr, "[uim] svc=%u inst=0x%x node=%u port=%u\n",
                    rp.server.service, rp.server.instance, rp.server.node, rp.server.port);
        if (!found) { g_uim_node = rp.server.node; g_uim_port = rp.server.port; found = 1; }
    }
    if (!found) { fprintf(stderr, "[!] no UIM service on QRTR bus\n"); return -1; }
    fprintf(stderr, "[uim] using node=%u port=%u\n", g_uim_node, g_uim_port);
    return 0;
}

static int qmi_send(int s, const uint8_t *pkt, size_t len) {
    struct sockaddr_qrtr dst = { .sq_family = AF_QIPCRTR, .sq_node = g_uim_node, .sq_port = g_uim_port };
    if (sendto(s, pkt, len, 0, (struct sockaddr*)&dst, sizeof(dst)) != (ssize_t)len) {
        perror("sendto"); return -1;
    }
    return 0;
}

/* Receive a QMI *response* (ctrl byte 0x02) for the given msgid; skip
 * indications (0x04) and stray ctrl traffic. */
static ssize_t qmi_recv(int s, uint8_t *buf, size_t bufsz, int timeout_ms, uint16_t want_msgid) {
    struct timeval tv = { .tv_sec = timeout_ms/1000, .tv_usec = (timeout_ms%1000)*1000 };
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    for (;;) {
        struct sockaddr_qrtr from = {0}; socklen_t fl = sizeof(from);
        ssize_t n = recvfrom(s, buf, bufsz, 0, (struct sockaddr*)&from, &fl);
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) { fprintf(stderr, "recv: timeout\n"); return -1; }
            perror("recvfrom"); return -1;
        }
        if (from.sq_node != g_uim_node || from.sq_port != g_uim_port) continue;
        if (n < 7) continue;
        uint16_t msgid = buf[3] | (buf[4] << 8);
        if (buf[0] == 0x02 && msgid == want_msgid) return n;
        /* indication or unrelated response — ignore */
    }
}

static int qmi_result(const uint8_t *body, size_t body_len, uint16_t *err_out) {
    uint16_t l = 0;
    const uint8_t *v = tlv_find(body, body_len, 0x02, &l);
    if (!v || l < 4) { fprintf(stderr, "[!] no result TLV\n"); return -1; }
    uint16_t result = v[0] | (v[1] << 8);
    uint16_t error  = v[2] | (v[3] << 8);
    if (err_out) *err_out = error;
    if (result != 0)
        fprintf(stderr, "[result] FAILURE qmi_error=%u\n", error);
    return result == 0 ? 0 : -1;
}

static int cmd_status(int s) {
    uint8_t pkt[16];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_GET_CARD_STATUS, 0);
    if (qmi_send(s, pkt, off) < 0) return 1;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 3000, UIM_GET_CARD_STATUS);
    if (r < 0) return 1;
    hexdump("< CARD_STATUS", rbuf, r);
    return qmi_result(rbuf + 7, r - 7, NULL) ? 1 : 0;
}

/* Open logical channel to ISD-R. Returns channel id >=0, or -1. */
static int isdr_open(int s) {
    uint8_t pkt[64];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_OPEN_LOGICAL_CHAN, 0);
    uint8_t slot = g_slot;
    off += tlv_add(pkt + off, 0x01, &slot, 1);
    uint8_t aid_tlv[1 + sizeof(ISDR_AID)];
    aid_tlv[0] = sizeof(ISDR_AID);
    memcpy(aid_tlv + 1, ISDR_AID, sizeof(ISDR_AID));
    off += tlv_add(pkt + off, 0x10, aid_tlv, sizeof(aid_tlv));
    uint16_t msglen = off - 7;
    pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;

    if (qmi_send(s, pkt, off) < 0) return -1;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, UIM_OPEN_LOGICAL_CHAN);
    if (r < 0) return -1;
    hexdump("< OPEN_LOGICAL_CHANNEL", rbuf, r);

    uint16_t qerr = 0;
    if (qmi_result(rbuf + 7, r - 7, &qerr)) {
        fprintf(stderr, "[!] open channel rejected by modem (qmi_error=%u)\n", qerr);
        return -1;
    }
    uint16_t l = 0;
    const uint8_t *v = tlv_find(rbuf + 7, r - 7, 0x10, &l);
    if (!v || l < 1) { fprintf(stderr, "[!] no channel id in response\n"); return -1; }
    int chan = v[0];
    const uint8_t *cr = tlv_find(rbuf + 7, r - 7, 0x11, &l);
    if (cr && l >= 2)
        fprintf(stderr, "[open] channel=%d SW=%02X%02X\n", chan, cr[0], cr[1]);
    else
        fprintf(stderr, "[open] channel=%d\n", chan);
    return chan;
}

/* Send one APDU on the channel; returns response length (incl. SW), -1 on error. */
static ssize_t uim_send_apdu(int s, int chan, const uint8_t *apdu, size_t alen,
                             uint8_t *out, size_t outcap) {
    uint8_t pkt[BUF_SZ];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_SEND_APDU, 0);
    uint8_t slot = g_slot;
    off += tlv_add(pkt + off, 0x01, &slot, 1);
    uint8_t apdu_tlv[2 + 300];
    apdu_tlv[0] = alen & 0xff; apdu_tlv[1] = alen >> 8;
    memcpy(apdu_tlv + 2, apdu, alen);
    off += tlv_add(pkt + off, 0x02, apdu_tlv, 2 + alen);
    uint8_t ch = (uint8_t)chan;
    off += tlv_add(pkt + off, 0x10, &ch, 1);
    uint16_t msglen = off - 7;
    pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;

    hexdump("> APDU", apdu, alen);
    if (qmi_send(s, pkt, off) < 0) return -1;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, UIM_SEND_APDU);
    if (r < 0) return -1;
    uint16_t qerr = 0;
    if (qmi_result(rbuf + 7, r - 7, &qerr)) {
        fprintf(stderr, "[!] send apdu rejected (qmi_error=%u)\n", qerr);
        return -1;
    }
    uint16_t l = 0;
    const uint8_t *v = tlv_find(rbuf + 7, r - 7, 0x10, &l);
    if (!v || l < 2) { fprintf(stderr, "[!] no APDU response TLV\n"); return -1; }
    uint16_t dlen = v[0] | (v[1] << 8);
    if (dlen > l - 2 || dlen > outcap) { fprintf(stderr, "[!] bad APDU resp len\n"); return -1; }
    memcpy(out, v + 2, dlen);
    hexdump("< APDU resp", out, dlen);
    return dlen;
}

static void isdr_close(int s, int chan) {
    uint8_t pkt[32];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_LOGICAL_CHANNEL, 0);
    uint8_t slot = g_slot;
    off += tlv_add(pkt + off, 0x01, &slot, 1);
    uint8_t ch = (uint8_t)chan;
    off += tlv_add(pkt + off, 0x11, &ch, 1);
    uint16_t msglen = off - 7;
    pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;
    if (qmi_send(s, pkt, off) < 0) return;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 3000, UIM_LOGICAL_CHANNEL);
    if (r >= 7) qmi_result(rbuf + 7, r - 7, NULL);
    fprintf(stderr, "[close] channel %d released\n", chan);
}

/* Read EF_ICCID (3F00/2FE2). session_type 0 = primary GW provisioning (what
 * the modem's NAS is actually using), 6 = card slot 1 (live card content). */
static int cmd_iccid(int s, uint8_t session_type) {
    uint8_t pkt[64];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_READ_TRANSPARENT, 0);
    uint8_t sess[2] = { session_type, 0 };           /* type, aid_len=0 */
    off += tlv_add(pkt + off, 0x01, sess, sizeof(sess));
    uint8_t file[5] = { 0xE2, 0x2F, 0x02, 0x00, 0x3F };  /* id=2FE2, path=3F00 */
    off += tlv_add(pkt + off, 0x02, file, sizeof(file));
    uint8_t rt[4] = { 0, 0, 0, 0 };                  /* offset=0 len=0 (all) */
    off += tlv_add(pkt + off, 0x03, rt, sizeof(rt));
    uint16_t msglen = off - 7;
    pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;
    if (qmi_send(s, pkt, off) < 0) return 1;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, UIM_READ_TRANSPARENT);
    if (r < 0) return 1;
    uint16_t qerr = 0;
    if (qmi_result(rbuf + 7, r - 7, &qerr)) {
        fprintf(stderr, "[iccid sess=%u] failed qmi_error=%u\n", session_type, qerr);
        return 1;
    }
    uint16_t l = 0;
    const uint8_t *v = tlv_find(rbuf + 7, r - 7, 0x11, &l);
    if (!v || l < 2) { fprintf(stderr, "[!] no read result\n"); return 1; }
    uint16_t dlen = v[0] | (v[1] << 8);
    printf("iccid(sess=%u)=", session_type);
    for (int i = 0; i < dlen && i < 10; i++) {
        uint8_t b = v[2 + i];
        printf("%c", '0' + (b & 0x0f));
        uint8_t hi = b >> 4;
        if (hi <= 9) printf("%c", '0' + hi);
    }
    printf("\n");
    return 0;
}

/* List apps on the card (from Get Card Status). Prints app type/state/AID. */
static int cmd_apps(int s, uint8_t *usim_aid_out, uint8_t *usim_aid_len) {
    uint8_t pkt[16];
    size_t off = qmi_build_header(pkt, g_txn++, UIM_GET_CARD_STATUS, 0);
    if (qmi_send(s, pkt, off) < 0) return 1;
    uint8_t rbuf[BUF_SZ];
    ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, UIM_GET_CARD_STATUS);
    if (r < 0) return 1;
    uint16_t l = 0;
    const uint8_t *v = tlv_find(rbuf + 7, r - 7, 0x10, &l);
    if (!v || l < 10) { fprintf(stderr, "[!] no card status TLV\n"); return 1; }
    /* u16 idx_gw_pri, u16 idx_1x_pri, u16 idx_gw_sec, u16 idx_1x_sec, u8 nslots */
    uint16_t idx_gw_pri = v[0] | (v[1] << 8);
    size_t p = 8;
    uint8_t nslots = v[p++];
    fprintf(stderr, "[cardstatus] gw_pri_index=0x%04x slots=%u\n", idx_gw_pri, nslots);
    for (uint8_t slot = 0; slot < nslots && p + 6 <= l; slot++) {
        uint8_t card_state = v[p], err_code = v[p+4], num_apps = v[p+5];
        p += 6;
        fprintf(stderr, "  slot%u: card_state=%u err=%u apps=%u\n", slot+1, card_state, err_code, num_apps);
        for (uint8_t a = 0; a < num_apps && p + 7 <= l; a++) {
            uint8_t app_type = v[p], app_state = v[p+1];
            uint8_t aid_len = v[p+6];
            const uint8_t *aid = v + p + 7;
            p += 7 + aid_len + 7;  /* skip pin block: upin_repl,pin1_st,pin1_rt,puk1_rt,pin2_st,pin2_rt,puk2_rt */
            fprintf(stderr, "    app%u: type=%u state=%u aid=", a, app_type, app_state);
            for (int i = 0; i < aid_len; i++) fprintf(stderr, "%02x", aid[i]);
            fprintf(stderr, "%s\n", (idx_gw_pri >> 8) == slot && (idx_gw_pri & 0xff) == a
                        ? "  <- GW provisioning app" : "");
            if (app_type == 2 /* USIM */ && slot == 0 && usim_aid_out && *usim_aid_len == 0) {
                memcpy(usim_aid_out, aid, aid_len);
                *usim_aid_len = aid_len;
            }
        }
    }
    return 0;
}

/* Rebind the primary GW provisioning session to the (new) USIM app:
 * deactivate, then activate with slot 1 + AID from card status. */
static int cmd_provision(int s) {
    uint8_t aid[32]; uint8_t aid_len = 0;
    if (cmd_apps(s, aid, &aid_len)) return 1;
    if (aid_len == 0) { fprintf(stderr, "[!] no USIM app found on slot 1\n"); return 1; }

    for (int step = 0; step < 2; step++) {
        uint8_t pkt[64];
        size_t off = qmi_build_header(pkt, g_txn++, UIM_CHANGE_PROV, 0);
        uint8_t sess[2] = { 0 /* primary GW */, (uint8_t)step /* 0=deactivate 1=activate */ };
        off += tlv_add(pkt + off, 0x01, sess, sizeof(sess));
        if (step == 1) {
            uint8_t appinfo[2 + 32];
            appinfo[0] = g_slot;
            appinfo[1] = aid_len;
            memcpy(appinfo + 2, aid, aid_len);
            off += tlv_add(pkt + off, 0x10, appinfo, 2 + aid_len);
        }
        uint16_t msglen = off - 7;
        pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;
        if (qmi_send(s, pkt, off) < 0) return 1;
        uint8_t rbuf[BUF_SZ];
        ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, UIM_CHANGE_PROV);
        if (r < 0) return 1;
        uint16_t qerr = 0;
        int rc = qmi_result(rbuf + 7, r - 7, &qerr);
        fprintf(stderr, "[prov %s] %s (qmi_error=%u)\n",
                step == 0 ? "deactivate" : "activate",
                rc == 0 ? "OK" : "FAILED", qerr);
        if (step == 0) usleep(300 * 1000);
    }
    return 0;
}

/* Power-cycle the SIM session so the modem re-reads the card after an eSIM
 * profile switch (the ZTE stack ignores the card's REFRESH). */
static int cmd_simreset(int s) {
    uint8_t rbuf[BUF_SZ];
    uint8_t slot = g_slot;
    for (int step = 0; step < 2; step++) {
        uint16_t msg = step == 0 ? UIM_POWER_DOWN : UIM_POWER_UP;
        uint8_t pkt[32];
        size_t off = qmi_build_header(pkt, g_txn++, msg, 0);
        off += tlv_add(pkt + off, 0x01, &slot, 1);
        uint16_t msglen = off - 7;
        pkt[5] = msglen & 0xff; pkt[6] = msglen >> 8;
        if (qmi_send(s, pkt, off) < 0) return 1;
        ssize_t r = qmi_recv(s, rbuf, sizeof(rbuf), 5000, msg);
        if (r < 0) return 1;
        uint16_t qerr = 0;
        int rc = qmi_result(rbuf + 7, r - 7, &qerr);
        fprintf(stderr, "[%s] %s (qmi_error=%u)\n",
                step == 0 ? "power_down" : "power_up",
                rc == 0 ? "OK" : "FAILED", qerr);
        if (rc != 0 && step == 0 && qerr != 0)
            fprintf(stderr, "    (continuing to power_up anyway)\n");
        if (step == 0) usleep(500 * 1000);
    }
    return 0;
}

static int cmd_eid(int s) {
    int chan = isdr_open(s);
    if (chan < 0) return 1;

    /* ES10c GetEID: STORE DATA BF3E035C015A, then GET RESPONSE while 61xx */
    uint8_t cla = 0x80 | (chan & 0x03);  /* channels 1-3 encode in CLA low bits */
    uint8_t apdu[300];
    size_t alen = 0;
    apdu[alen++] = cla; apdu[alen++] = 0xE2; apdu[alen++] = 0x91; apdu[alen++] = 0x00;
    apdu[alen++] = 0x06;
    apdu[alen++] = 0xBF; apdu[alen++] = 0x3E; apdu[alen++] = 0x03;
    apdu[alen++] = 0x5C; apdu[alen++] = 0x01; apdu[alen++] = 0x5A;
    apdu[alen++] = 0x00;

    uint8_t acc[1024]; size_t acc_len = 0;
    uint8_t resp[512];
    int ok = 0;
    for (int round = 0; round < 8; round++) {
        ssize_t rl = uim_send_apdu(s, chan, apdu, alen, resp, sizeof(resp));
        if (rl < 2) break;
        uint8_t sw1 = resp[rl-2], sw2 = resp[rl-1];
        if (rl > 2) { memcpy(acc + acc_len, resp, rl - 2); acc_len += rl - 2; }
        if (sw1 == 0x61) {
            alen = 0;
            apdu[alen++] = cla; apdu[alen++] = 0xC0; apdu[alen++] = 0x00;
            apdu[alen++] = 0x00; apdu[alen++] = sw2;
            continue;
        }
        if (sw1 == 0x90 && sw2 == 0x00) { ok = 1; break; }
        fprintf(stderr, "[!] unexpected SW=%02X%02X\n", sw1, sw2);
        break;
    }
    isdr_close(s, chan);
    if (!ok || acc_len < 4) { fprintf(stderr, "[!] GetEID failed\n"); return 1; }

    hexdump("[es10c] GetEID response", acc, acc_len);
    /* Expect BF3E xx 5A 10 <16-byte EID> */
    for (size_t i = 0; i + 18 <= acc_len; i++) {
        if (acc[i] == 0x5A && acc[i+1] == 0x10) {
            printf("EID=");
            for (int j = 0; j < 16; j++) printf("%02X", acc[i+2+j]);
            printf("\n");
            return 0;
        }
    }
    fprintf(stderr, "[!] EID tag not found in response\n");
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: %s lookup|status|open|eid|simreset|iccid|apps|provision [slot]\n", argv[0]);
        return 2;
    }
    /* Optional trailing slot arg (1-based) for open/eid/provision. Default 1. */
    if (argc >= 3) {
        int sl = atoi(argv[2]);
        if (sl >= 1 && sl <= 8) g_slot = (uint8_t)sl;
    }
    int s = qrtr_open();
    if (s < 0) return 1;
    if (uim_lookup(s, !strcmp(argv[1], "lookup")) < 0) return 1;

    int rc = 1;
    if (!strcmp(argv[1], "lookup")) rc = 0;
    else if (!strcmp(argv[1], "status")) rc = cmd_status(s);
    else if (!strcmp(argv[1], "open")) { int c = isdr_open(s); if (c >= 0) { isdr_close(s, c); rc = 0; } }
    else if (!strcmp(argv[1], "eid")) rc = cmd_eid(s);
    else if (!strcmp(argv[1], "simreset")) rc = cmd_simreset(s);
    else if (!strcmp(argv[1], "iccid")) { int a = cmd_iccid(s, 6), b = cmd_iccid(s, 0); rc = a || b; }
    else if (!strcmp(argv[1], "apps")) { uint8_t dummy[32], dl = 0; rc = cmd_apps(s, dummy, &dl); }
    else if (!strcmp(argv[1], "provision")) rc = cmd_provision(s);
    else fprintf(stderr, "unknown cmd\n");
    close(s);
    return rc;
}
