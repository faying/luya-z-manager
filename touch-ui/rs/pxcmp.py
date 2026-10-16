#!/usr/bin/env python3
"""Exact pixel compare of two PNGs (the C render test's and the Rust UI's):

  pxcmp.py REF.png OUT.png     prints how many pixels differ and their box; exit 1 if any

Standard library only. (spike/inkcmp.py, not public, also says where things are offset.)
SPDX-License-Identifier: MIT
"""
import struct
import sys
import zlib


def png(path):
    d = open(path, "rb").read()
    assert d[:8] == b"\x89PNG\r\n\x1a\n", path
    i, idat, w, h, ct = 8, b"", 0, 0, 0
    while i < len(d):
        n, kind = struct.unpack(">I4s", d[i : i + 8])
        body = d[i + 8 : i + 8 + n]
        if kind == b"IHDR":
            w, h, bd, ct = struct.unpack(">IIBB", body[:10])
            assert bd == 8, "8-bit only"
        elif kind == b"IDAT":
            idat += body
        i += 12 + n
    bpp = {2: 3, 6: 4}[ct]
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    p = 0
    for _ in range(h):
        f = raw[p]
        line = bytearray(raw[p + 1 : p + 1 + stride])
        p += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append([tuple(line[x * bpp : x * bpp + 3]) for x in range(w)])
        prev = line
    return rows


def exact(ref_path, out_path):
    ref, out = png(ref_path), png(out_path)
    n, box = 0, None
    for y, (ra, rb) in enumerate(zip(ref, out)):
        for x, (p, q) in enumerate(zip(ra, rb)):
            if p != q:
                n += 1
                box = (x, y, x, y) if not box else (min(box[0], x), min(box[1], y), max(box[2], x), max(box[3], y))
    print(f"{out_path.rsplit('/', 1)[-1]}: {n} pixels differ" + (f", x {box[0]}-{box[2]} y {box[1]}-{box[3]}" if n else ""))
    return 1 if n else 0


if __name__ == "__main__":
    sys.exit(exact(sys.argv[1], sys.argv[2]))
