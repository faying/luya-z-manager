#!/usr/bin/env python3
"""Fetch the Alpine packages that make up the eSIM (lpac) bundle.

Modes:
  (no argument)  make the local cache match alpine.lock: every package named
                 there is downloaded unless a cached copy already has the
                 recorded sha256; any mismatch is fatal
  --relock       walk lpac's shared-library dependencies in the APKINDEX of
                 $ALPINE_BRANCH (default v3.24) and write a fresh alpine.lock.
                 A new lock must be built and tried on a real device
                 (lpac.sh chip info) before it is committed.

Environment: ALPINE_PKGS_DIR (cache, default ./alpine-pkgs next to this file),
ALPINE_MIRROR (default the Alpine CDN), ALPINE_BRANCH (--relock only).

The lock follows a stable branch on purpose: edge rebuilds daily and drops old
files. When a stable branch replaces a pinned file after a security fix, the
plain mode stops with an error and --relock is the way forward.

Lock format, one package per line (lines starting with # are comments):
  <branch>/<repo> <name> <version> <sha256 of the .apk>
"""
import hashlib
import io
import os
import sys
import tarfile
import urllib.request

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
LOCK_PATH = os.path.join(SCRIPT_DIR, "alpine.lock")
CACHE_DIR = os.environ.get("ALPINE_PKGS_DIR") or os.path.join(SCRIPT_DIR, "alpine-pkgs")
MIRROR = os.environ.get("ALPINE_MIRROR") or "https://dl-cdn.alpinelinux.org/alpine"
TARGET_ARCH = "aarch64"
REPOS = ("community", "main")
ROOT_PACKAGE = "lpac"


def http_get(url):
    with urllib.request.urlopen(url, timeout=120) as resp:
        return resp.read()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def apk_url(branch_repo, filename):
    return "/".join((MIRROR.rstrip("/"), branch_repo, TARGET_ARCH, filename))


# ---------------------------------------------------------------- lock sync

def read_lock():
    entries = []
    with open(LOCK_PATH) as fh:
        for raw in fh:
            text = raw.strip()
            if not text or text.startswith("#"):
                continue
            branch_repo, name, version, sha = text.split()
            entries.append((branch_repo, name, version, sha))
    return entries


def cached_ok(path, sha):
    if not os.path.isfile(path):
        return False
    with open(path, "rb") as fh:
        return digest(fh.read()) == sha


def sync_cache():
    os.makedirs(CACHE_DIR, exist_ok=True)
    for branch_repo, name, version, sha in read_lock():
        filename = "%s-%s.apk" % (name, version)
        target = os.path.join(CACHE_DIR, filename)
        if cached_ok(target, sha):
            continue
        print("downloading %s/%s" % (branch_repo, filename))
        try:
            blob = http_get(apk_url(branch_repo, filename))
        except Exception as exc:  # network error, 404 after a replacement, ...
            sys.exit("ERROR: cannot download %s (%s). The pinned package was probably "
                     "replaced upstream: run alpine_closure.py --relock, then test the "
                     "new bundle on a device." % (filename, exc))
        if digest(blob) != sha:
            sys.exit("ERROR: %s does not match the sha256 in alpine.lock" % filename)
        with open(target, "wb") as fh:
            fh.write(blob)
    print("OK: all packages match alpine.lock")


# ------------------------------------------------------------------ relock

def index_records(text):
    """Yield one dict per APKINDEX stanza (single-letter keys, first value wins)."""
    for stanza in text.split("\n\n"):
        record = {}
        for line in stanza.splitlines():
            if len(line) >= 3 and line[1] == ":":
                record.setdefault(line[0], line[2:])
        if "P" in record:
            yield record


def load_index(branch):
    """Return (packages, so_providers) over all repos; earlier repos win."""
    packages, providers = {}, {}
    for repo in REPOS:
        archive = http_get(apk_url("%s/%s" % (branch, repo), "APKINDEX.tar.gz"))
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
            text = tar.extractfile("APKINDEX").read().decode()
        for rec in index_records(text):
            so_deps = [d for d in rec.get("D", "").split() if d.startswith("so:")]
            packages.setdefault(rec["P"], {"repo": repo, "version": rec["V"], "needs": so_deps})
            for item in rec.get("p", "").split():
                soname = item.split("=", 1)[0]
                if soname.startswith("so:"):
                    providers.setdefault(soname, rec["P"])
    return packages, providers


def relock(branch):
    packages, providers = load_index(branch)
    pending, seen, lines = [ROOT_PACKAGE], set(), []
    while pending:
        name = pending.pop()
        if name in seen or name not in packages:
            continue
        seen.add(name)
        info = packages[name]
        branch_repo = "%s/%s" % (branch, info["repo"])
        blob = http_get(apk_url(branch_repo, "%s-%s.apk" % (name, info["version"])))
        lines.append((name, "%s %s %s %s" % (branch_repo, name, info["version"], digest(blob))))
        for soname in info["needs"]:
            if "musl" in soname:  # libc comes from the device itself
                continue
            owner = providers.get(soname)
            if owner is None:
                print("WARN: nothing provides %s (wanted by %s)" % (soname, name), file=sys.stderr)
            else:
                pending.append(owner)
    lines.sort()
    with open(LOCK_PATH, "w") as fh:
        fh.write("# lpac closure for aarch64, generated by alpine_closure.py --relock (%s)\n" % branch)
        fh.write("# branch/repo  package  version  sha256(.apk)\n")
        for _, line in lines:
            fh.write(line + "\n")
    print("wrote %s (%d packages)" % (LOCK_PATH, len(lines)))


def main(argv):
    if argv[:1] == ["--relock"]:
        relock(os.environ.get("ALPINE_BRANCH") or "v3.24")
    else:
        sync_cache()


if __name__ == "__main__":
    main(sys.argv[1:])
