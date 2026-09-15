#!/usr/bin/env python3
"""Resolve and download the Alpine package closure for lpac's shared-lib deps."""
import io, os, re, sys, tarfile, urllib.request

MIRROR = "http://dl-cdn.alpinelinux.org/alpine/edge"
REPOS = ["community", "main"]
ARCH = "aarch64"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "alpine-pkgs")

def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read()

# repo -> list of package dicts
pkgs = {}          # name -> (repo, name, ver, deps[list of so: names])
provides = {}      # so:name -> pkgname

for repo in REPOS:
    raw = fetch(f"{MIRROR}/{repo}/{ARCH}/APKINDEX.tar.gz")
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tf:
        idx = tf.extractfile("APKINDEX").read().decode()
    for block in idx.split("\n\n"):
        f = {}
        for line in block.splitlines():
            if len(line) > 2 and line[1] == ":":
                f.setdefault(line[0], line[2:])
        if "P" not in f:
            continue
        name, ver = f["P"], f["V"]
        deps = [d for d in f.get("D", "").split() if d.startswith("so:")]
        if name not in pkgs:
            pkgs[name] = (repo, name, ver, deps)
        for p in f.get("p", "").split():
            p = p.split("=")[0]
            if p.startswith("so:") and p not in provides:
                provides[p] = name

start = sys.argv[1] if len(sys.argv) > 1 else "lpac"
os.makedirs(OUT, exist_ok=True)

resolved, queue, downloads = set(), [start], []
while queue:
    name = queue.pop()
    if name in resolved or name not in pkgs:
        continue
    resolved.add(name)
    repo, _, ver, deps = pkgs[name]
    downloads.append((repo, name, ver))
    for d in deps:
        if "musl" in d:   # libc provided by device
            continue
        prov = provides.get(d)
        if prov:
            queue.append(prov)
        else:
            print(f"WARN: no provider for {d} (needed by {name})", file=sys.stderr)

for repo, name, ver in downloads:
    fn = f"{name}-{ver}.apk"
    dest = os.path.join(OUT, fn)
    if not os.path.exists(dest):
        print(f"fetch {repo}/{fn}")
        open(dest, "wb").write(fetch(f"{MIRROR}/{repo}/{ARCH}/{fn}"))
print("DONE:", " ".join(n for _, n, _ in downloads))
