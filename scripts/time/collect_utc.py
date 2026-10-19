#!/usr/bin/env python3
"""Read-only NTS gateway acquisition. Writes LOCAL evidence only; never sets any clock.
Requires an existing, trusted chronyd/chronyc. No installation or device write mode.
Provisioning the resulting anchor over authenticated root SSH is a separate approved step.
"""
import argparse
import datetime as dt
import hashlib
import ipaddress
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import time


class Unverified(ValueError):
    pass


def fields(text):
    return {k.strip(): v.strip() for line in text.splitlines() if ':' in line
            for k, v in [line.split(':', 1)]}


def number(text):
    try:
        value = float(text.split()[0])
    except (ValueError, IndexError) as e:
        raise Unverified('missing/invalid numeric evidence') from e
    if not math.isfinite(value) or value < 0:
        raise Unverified('non-finite/negative error bound')
    return value


def validate_tracking(text, host_now):
    f = fields(text)
    if f.get('Leap status') != 'Normal' or not 1 <= int(f.get('Stratum', '0')) <= 15:
        raise Unverified('source not normally synchronised')
    reference = f.get('Reference ID', '').split()
    if not reference:
        raise Unverified('missing tracking source identity')
    if reference[0] == '7F7F0101':
        raise Unverified('local free-running reference')
    try:
        reference_time = dt.datetime.strptime(f['Ref time (UTC)'], '%a %b %d %H:%M:%S %Y').replace(tzinfo=dt.timezone.utc).timestamp()
    except (KeyError, ValueError) as e:
        raise Unverified('missing reference time') from e
    # A small host/source offset is a prerequisite, not repaired by this program.
    error = number(f.get('System time', '')) + number(f.get('Root dispersion', '')) + number(f.get('Root delay', '')) / 2
    if error > 1 or not -1 <= host_now - reference_time <= 180:
        raise Unverified('source stale or error exceeds 1 second')
    return error


def selected_source(text):
    rows = [line.split() for line in text.splitlines() if line.startswith('^*')]
    if len(rows) != 1:
        raise Unverified('no unique selected network source')
    return str(ipaddress.ip_address(rows[0][1]))


def authenticate(source, auth, ntpdata, tracking):
    rows = [line.split() for line in auth.splitlines() if line.split() and line.split()[0] == source]
    n = fields(ntpdata)
    remote = n.get('Remote address', '').split()
    if len(rows) != 1 or rows[0][1] != 'NTS' or n.get('Authenticated') != 'Yes':
        raise Unverified('selected source lacks authenticated NTS measurement')
    if not remote or remote[0] != source or n.get('Leap status') != 'Normal':
        raise Unverified('selected and authenticated source differ')
    ref = fields(tracking).get('Reference ID', '').split()
    remote_ref = n.get('Remote address', '').split('(')[-1].rstrip(')')
    if not ref or ref[0] != remote_ref:
        raise Unverified('tracking reference changed or mismatched')
    if n.get('NTP tests') != '111 111 1111':
        raise Unverified('NTP measurement tests failed/unknown')


def build_anchor(report):
    """Pure verifier; the trusted gateway must preserve the original command evidence."""
    start, end = report['start'], report['end']
    elapsed = end['mono'] - start['mono']
    if not math.isfinite(elapsed) or not 0 <= elapsed <= 10:
        raise Unverified('acquisition must finish within 10 seconds')
    if abs((end['wall'] - start['wall']) - elapsed) > 0.25:
        raise Unverified('gateway wall clock stepped during acquisition')
    source = selected_source(report['sources_before'])
    if selected_source(report['sources_after']) != source:
        raise Unverified('selected source changed during acquisition')
    bounds = [validate_tracking(report[k], start['wall'] if k.endswith('before') else end['wall'])
              for k in ('tracking_before', 'tracking_after')]
    authenticate(source, report['authdata'], report['ntpdata'], report['tracking_before'])
    if fields(report['tracking_before'])['Reference ID'] != fields(report['tracking_after'])['Reference ID']:
        raise Unverified('tracking reference changed during acquisition')
    lines = report['device'].splitlines()
    if len(lines) != 7 or lines[0] != lines[5]:
        raise Unverified('device boot identity missing/changed')
    boot = lines[0]
    if not boot or len(boot) > 64 or any(c not in '0123456789abcdefABCDEF-' for c in boot):
        raise Unverified('invalid boot identity')
    ups = [float(lines[i].split()[0]) for i in (1, 6)]
    walls = [int(lines[i]) for i in (2, 4)]
    if not all(math.isfinite(u) and u >= 0 for u in ups) or not 0 <= ups[1] - ups[0] <= elapsed + 1:
        raise Unverified('invalid device elapsed bracket')
    if abs((walls[1] - walls[0]) - (ups[1] - ups[0])) > 2:
        raise Unverified('device wall stepped during acquisition')
    zone = float(lines[3]) * 3600
    if not math.isfinite(zone) or not -43200 <= zone <= 50400 or zone != int(zone):
        raise Unverified('display zone unknown')
    # No half-RTT accuracy assumption: full collection interval + two quantised seconds.
    uncertainty = math.ceil(max(bounds) + elapsed + 2)
    if uncertainty > 4:
        raise Unverified('acquisition uncertainty exceeds 4s (1s reserved for drift)')
    estimate = round((start['wall'] + end['wall']) / 2)
    delta = walls[0] - estimate
    near_utc, near_local = abs(delta) <= uncertainty, abs(delta - zone) <= uncertainty
    if not ((near_utc and (not near_local or zone == 0)) or (near_local and not near_utc)):
        raise Unverified('device epoch model wrong or ambiguous')
    digest = hashlib.sha256(json.dumps(report, sort_keys=True, allow_nan=False).encode()).hexdigest()
    return dict(version=1, source='chrony_nts', source_id=source, evidence_sha256=digest,
                observed_at=dict(boot_id=boot, seconds=math.floor(ups[0])), raw_wall=walls[0],
                utc_estimate=estimate, uncertainty_s=uncertainty, valid_for_s=900,
                drift_ppm=1000, display_zone=int(zone))


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=12,
                          env={**os.environ, 'LC_ALL': 'C'}).stdout


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--host', default='u60', help='existing verified SSH alias')
    ap.add_argument('--out', type=Path, required=True, help='new local evidence directory')
    args = ap.parse_args()
    if not shutil.which('chronyc'):
        ap.exit(2, 'Unverified: chronyc unavailable; install/configuration requires separate approval. No device contacted.\n')
    if not args.host or args.host.startswith('-'):
        ap.error('invalid SSH alias')
    args.out.mkdir(mode=0o700, parents=True, exist_ok=False)
    report = {'start': {'wall': time.time(), 'mono': time.monotonic()}}
    try:
        report['tracking_before'] = run(['chronyc', '-n', 'tracking'])
        report['sources_before'] = run(['chronyc', '-n', 'sources'])
        source = selected_source(report['sources_before'])
        report['authdata'] = run(['chronyc', '-n', 'authdata'])
        report['ntpdata'] = run(['chronyc', '-n', 'ntpdata', source])
        # Strict host-key verification, read-only getters, no token/config writes.
        command = ('cat /proc/sys/kernel/random/boot_id /proc/uptime; date -u +%s; '
                   'uci -q get zwrt_zte_sntp.settings.time_from_utc; date -u +%s; '
                   'cat /proc/sys/kernel/random/boot_id /proc/uptime')
        report['device'] = run(['ssh', '-n', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', args.host, command])
        report['sources_after'] = run(['chronyc', '-n', 'sources'])
        report['tracking_after'] = run(['chronyc', '-n', 'tracking'])
        report['end'] = {'wall': time.time(), 'mono': time.monotonic()}
        anchor = build_anchor(report)
    except (Unverified, ValueError, KeyError, subprocess.SubprocessError) as e:
        report['error'] = str(e)
        (args.out / 'report.json').write_text(json.dumps(report, indent=2, allow_nan=False))
        ap.exit(2, f'Unverified: {e}; no anchor produced.\n')
    (args.out / 'report.json').write_text(json.dumps(report, indent=2, allow_nan=False))
    (args.out / 'utc-anchor.json').write_text(json.dumps(anchor) + '\n')
    print(f'Local evidence ready: {args.out}; no device writes. Anchor expires in 900s of this boot.')


if __name__ == '__main__':
    main()
