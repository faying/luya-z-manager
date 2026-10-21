#!/usr/bin/env python3
"""Writes a synthetic step-0 capture (made-up clients, no real data) for test.sh."""

import struct
import sys


def eth_ip_udp(src_mac, src_ip, dst_ip, sp, dp, payload, dst_mac=b"\xff" * 6):
    udp = struct.pack("!HHHH", sp, dp, 8 + len(payload), 0) + payload
    ip = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(udp), 0, 0, 64, 17, 0, bytes(src_ip), bytes(dst_ip))
    return dst_mac + bytes(src_mac) + b"\x08\x00" + ip + udp


def dhcp_req(mac, opts):
    p = bytearray(240)
    p[0], p[1], p[2] = 1, 1, 6
    p[28:34] = bytes(mac)
    p[236:240] = b"\x63\x82\x53\x63"
    for code, v in opts:
        p += bytes([code, len(v)]) + v
    return bytes(p + b"\xff")


def label(name):
    return b"".join(bytes([len(x)]) + x.encode() for x in name.split(".")) + b"\x00"


def mdns_announce(inst, svc, host, model):
    rr = b""
    rr += label(svc) + struct.pack("!HHIH", 12, 1, 120, len(label(f"{inst}.{svc}"))) + label(f"{inst}.{svc}")
    rr += label(f"{host}.local") + struct.pack("!HHIH", 1, 1, 120, 4) + bytes([192, 168, 0, 9])
    txt = bytes([len(f"model={model}")]) + f"model={model}".encode()
    rr += label(f"{inst}._device-info._tcp.local") + struct.pack("!HHIH", 16, 1, 120, len(txt)) + txt
    return struct.pack("!HHHHHH", 0, 0x8400, 0, 3, 0, 0) + rr


def mdns_loop():
    # one answer whose name points at itself
    return struct.pack("!HHHHHH", 0, 0x8400, 0, 1, 0, 0) + b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 1, 4) + b"\0\0\0\0"


def nb_name(name):
    raw = name.upper().ljust(15).encode()[:15] + b"\x00"
    return b"\x20" + b"".join(bytes([0x41 + (c >> 4), 0x41 + (c & 15)]) for c in raw) + b"\x00"


def nbns_query(name):
    return struct.pack("!HHHHHH", 1, 0x0110, 1, 0, 0, 0) + nb_name(name) + struct.pack("!HH", 32, 1)


def nbns_reg(name, group=False):
    # opcode 5 (registration), one question + one additional NB record
    rr = b"\xc0\x0c" + struct.pack("!HHIH", 32, 1, 300000, 6) + struct.pack("!H", 0x8000 if group else 0) + bytes([192, 168, 0, 9])
    return struct.pack("!HHHHHH", 2, 0x2910, 1, 0, 0, 1) + nb_name(name) + struct.pack("!HH", 32, 1) + rr


def mdns_query_known_answer(svc, other_inst):
    # a query (QR clear) with a known answer about another device
    rr = label(svc) + struct.pack("!HHIH", 12, 1, 120, len(label(f"{other_inst}.{svc}"))) + label(f"{other_inst}.{svc}")
    return struct.pack("!HHHHHH", 0, 0, 1, 1, 0, 0) + label(svc) + struct.pack("!HH", 12, 1) + rr


PHONE = b"\x02\x11\x22\x33\x44\x01"  # random MAC, DHCP host name only
MAC = b"\x3c\x22\xfb\x00\x00\x02"  # OUI MAC, mDNS + NetBIOS
TV = b"\x00\x09\xb0\x00\x00\x03"  # SSDP only
BAD = b"\x02\xde\xad\x00\x00\x04"  # malformed mDNS

frames = [
    eth_ip_udp(PHONE, [0] * 4, [255] * 4, 68, 67, dhcp_req(PHONE, [(53, b"\x01"), (12, b"Pixel-9"), (55, bytes([1, 3, 6, 15, 26, 28, 51, 58, 59, 43]))])),
    eth_ip_udp(MAC, [0] * 4, [255] * 4, 68, 67, dhcp_req(MAC, [(53, b"\x03"), (12, b"mbp"), (60, b""), (55, bytes([1, 121, 3, 6, 15, 108, 114, 119, 252, 95, 44, 46]))])),
    eth_ip_udp(MAC, [192, 168, 0, 9], [224, 0, 0, 251], 5353, 5353, mdns_announce("Test MacBook Pro", "_companion-link._tcp.local", "Test-MBP", "Mac14,2")),
    eth_ip_udp(MAC, [192, 168, 0, 9], [192, 168, 0, 255], 137, 137, nbns_reg("TEST-MBP")),
    eth_ip_udp(TV, [192, 168, 0, 10], [239, 255, 255, 250], 1900, 1900,
               b"NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNT: upnp:rootdevice\r\nSERVER: Linux/4.4 UPnP/1.0 Roku/12.5\r\n\r\n"),
    eth_ip_udp(BAD, [192, 168, 0, 11], [224, 0, 0, 251], 5353, 5353, mdns_loop()),
    eth_ip_udp(PHONE, [192, 168, 0, 8], [224, 0, 0, 251], 5353, 5353, mdns_query_known_answer("_airplay._tcp.local", "Living Room TV")),
    eth_ip_udp(PHONE, [192, 168, 0, 8], [192, 168, 0, 255], 137, 137, nbns_query("WPAD")),
    eth_ip_udp(MAC, [192, 168, 0, 9], [192, 168, 0, 255], 137, 137, nbns_reg("WORKGROUP", group=True)),
    b"\x00" * 10,  # runt
]

with open(sys.argv[1], "wb") as f:
    f.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 1514, 1))
    for i, fr in enumerate(frames):
        f.write(struct.pack("<IIII", 1700000000 + i, 0, len(fr), len(fr)) + fr)
