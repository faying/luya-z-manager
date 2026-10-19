#!/usr/bin/env python3
"""Read-only firmware baseline via an existing SSH alias. Never installs or opens AT.

No raw replies are retained after capture. Only reviewed fields, method listings and hashes leave memory.
This is an offline historical capture, never a live capability/epoch importer.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import subprocess
import tempfile
from datetime import datetime, timezone

# Fixed commands, no user-provided object/path/command interpolated into remote shell.
COMMANDS = {
    'boot_start': 'cat /proc/sys/kernel/random/boot_id',
    'uptime_start': 'cat /proc/uptime',
    'device_wall': 'date +%Y-%m-%dT%H:%M:%S%z',
    'common': "ubus -t 3 call zwrt_zte_mdm.api get_zwrt_common_info '{}'",
    'board': "ubus -t 3 call system board '{}'",
    'usb_methods': 'ubus -t 3 -v list zwrt_bsp.usb',
    'typec_methods': 'ubus -t 3 -v list zwrt_bsp.typec',
    'powerbank_methods': 'ubus -t 3 -v list zwrt_bsp.powerbank',
    'usb': "ubus -t 3 call zwrt_bsp.usb list '{}'",
    'typec': "ubus -t 3 call zwrt_bsp.typec list '{}'",
    'wwan': "ubus -t 3 call zwrt_data get_wwaniface '{\"source_module\":\"web\",\"cid\":1,\"connect_status\":\"\"}'",
    'battery': "ubus -t 3 call zwrt_bsp.battery list '{}'",
    'charger': "ubus -t 3 call zwrt_bsp.charger list '{}'",
    'radio': 'uci -q get zte_nwinfo.sys_info.operate_mode',
    'external': 'uci -q get zwrt_common_info.common_config.integrate_version',
    'internal': 'uci -q get zwrt_common_info.common_config.wa_inner_version',
    'fota_mode': 'uci -q get zwrt_zte_dm.dm_update.dm_update_mode',
    'fota_poll': 'uci -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING',
    'udc': 'for p in /sys/class/udc/*/state; do [ ! -r "$p" ] || cat "$p"; done',
    'uptime_end': 'cat /proc/uptime',
    'boot_end': 'cat /proc/sys/kernel/random/boot_id',
}
FIELDS = {
    'common': ('manufacturer', 'model_name', 'hardware_version', 'integrate_version', 'wa_inner_version'),
    'board': ('board_name', 'kernel'),
    'usb': ('mode',),
    'typec': ('cc_attch_state', 'data_role', 'power_role'),
    'battery': ('battery_capacity', 'battery_temperature', 'battery_online', 'battery_health', 'battery_time_to_full'),
    'wwan': ('connect_status', 'enable', 'ipv4_dev_name', 'ipv6_dev_name'),
    'charger': ('charge_status', 'charger_connect', 'charger_type', 'direct_power_supply_mode'),
}
MAX_SECTION = 65536


def script():
    rows = ['export LC_ALL=C']
    for key, command in COMMANDS.items():
        rows += [f"printf 'LUYAZ_BEGIN {key}\\n'", f'({command}) 2>/dev/null',
                 "rc=$?", f"printf '\\nLUYAZ_END {key} %s\\n' \"$rc\""]
    rows += ["printf 'LUYAZ_COMPLETE\\n'"]
    return '\n'.join(rows) + '\n'


def decode(raw):
    if len(raw) > 1024 * 1024:
        raise ValueError('capture exceeds 1 MiB')
    text = raw.decode('utf-8', errors='strict')
    sections = {}
    pos = 0
    for key in COMMANDS:
        begin = f'LUYAZ_BEGIN {key}\n'
        if not text.startswith(begin, pos):
            raise ValueError(f'missing or reordered section: {key}')
        pos += len(begin)
        end = re.search(rf'\nLUYAZ_END {key} ([0-9]+)\n', text[pos:])
        if not end:
            raise ValueError(f'incomplete section: {key}')
        body = text[pos:pos+end.start()]
        if len(body.encode()) > MAX_SECTION:
            raise ValueError(f'oversize section: {key}')
        sections[key] = (int(end[1]), body)
        pos += end.end()
    if text[pos:] != 'LUYAZ_COMPLETE\n':
        raise ValueError('incomplete capture or trailing output')
    return sections


def sanitized(sections, cohort):
    entries = {}
    for key, (rc, body) in sections.items():
        item = {'exit_code': rc, 'raw_sha256': hashlib.sha256(body.encode()).hexdigest(),
                'value': None}
        if rc == 0:
            if key in FIELDS:
                try:
                    v = json.loads(body)
                    if not isinstance(v, dict):
                        raise ValueError('expected object')
                    item['value'] = {k: v[k] for k in FIELDS[key] if k in v and
                                     isinstance(v[k], (str, int, float, bool)) and
                                     len(str(v[k])) <= 256 and not any(ord(c) < 32 for c in str(v[k]))}
                    if key == 'board' and isinstance(v.get('release'), dict):
                        item['value']['release'] = {k: v['release'][k] for k in ('description', 'revision')
                                                  if isinstance(v['release'].get(k), str) and len(v['release'][k]) <= 256}
                except (ValueError, TypeError):
                    item['decode_error'] = 'invalid object'
            elif key.endswith('_methods'):
                # Preserve method grammar but erase process-specific object handle.
                # Only reviewed object headers and method signatures are retained.
                expected = {'usb_methods':'zwrt_bsp.usb', 'typec_methods':'zwrt_bsp.typec',
                            'powerbank_methods':'zwrt_bsp.powerbank'}[key]
                lines = body.strip().splitlines()
                if lines and re.fullmatch(rf"'{re.escape(expected)}' @[0-9a-fA-F]+", lines[0]):
                    methods = []
                    try:
                        seen = set()
                        for line in lines[1:]:
                            pairs = json.loads('{' + line.strip() + '}', object_pairs_hook=list)
                            if len(pairs) != 1 or pairs[0][0] in seen or not isinstance(pairs[0][1], list):
                                raise ValueError('malformed signature')
                            name, signature = pairs[0]
                            if len({k for k, _ in signature}) != len(signature):
                                raise ValueError("duplicate signature field")
                            if not re.fullmatch('[A-Za-z0-9_]+', name):
                                raise ValueError('invalid method')
                            if any(not re.fullmatch('[A-Za-z0-9_]+', k) or not isinstance(t, str) or
                                   t not in ('String','Integer','Boolean','Array','Table','(unknown)') for k, t in signature):
                                raise ValueError('unexpected signature')
                            seen.add(name)
                            methods.append({'name':name, 'signature':dict(signature)})
                        if not methods:
                            raise ValueError('header alone is not complete evidence')
                        item['value'] = {'object':expected, 'methods':methods}
                    except (ValueError, TypeError):
                        item['decode_error'] = 'unrecognized complete method grammar'
                else:
                    item['decode_error'] = 'unrecognized listing header'
            else:
                value = body.strip()
                if len(value) <= 256 and not any(ord(c) < 32 for c in value):
                    item['value'] = value
        entries[key] = item
    boot = entries['boot_start']['value']
    boot_ok = bool(boot and re.fullmatch('[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}', boot) and boot == entries['boot_end']['value'])
    elapsed_ok = False
    try:
        start, end = (float(entries[k]['value'].split()[0]) for k in ('uptime_start','uptime_end'))
        elapsed_ok = 0 <= end-start <= 90
    except (ValueError, TypeError, AttributeError):
        pass
    fota = entries['fota_mode']['value'] == '0' and entries['fota_poll']['value'] == '1'
    return {'schema_version':1, 'runtime_loadable':False, 'cohort':cohort,
            'captured_at_host_utc':datetime.now(timezone.utc).isoformat(),
            'device_utc_quality':'unknown', 'same_boot':boot_ok,
            'bounded_elapsed':elapsed_ok, 'fota_off':fota,
            'entries':entries, 'limitations':[
                'Historical read-only snapshot; no live epoch or write support is established.',
                'Cohort is operator-declared, not inferred from firmware suffix.',
                'No AT access, reboot, installation, ADB enablement, config backup or write verification.',
                'Web/Touch displayed time and native-device baseline require separate observation.',
                'A snapshot cannot prove event subscription coverage, no loss or ABA protection.',
            ]}


def bound_output():
    # Applied only in the SSH child, not to the caller or device. Prevent a bad remote
    # program filling the host filesystem while preserving the no-raw-retention rule.
    resource.setrlimit(resource.RLIMIT_FSIZE, (1024*1024,1024*1024))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ssh', required=True, help='existing OpenSSH config alias')
    parser.add_argument('--cohort', choices=('upgraded','native','unknown'), required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch('[A-Za-z0-9][A-Za-z0-9_.-]{0,63}', args.ssh):
        parser.error('use an existing SSH alias, not an address or option')
    if args.out.exists():
        parser.error('output already exists; choose a new capture path')
    # Private local temporary files bound process memory. No raw response persists after exit.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        result = subprocess.run(['ssh','-T','-o','BatchMode=yes','-o','ConnectTimeout=8',args.ssh,'sh -s'],
                                input=script().encode(), stdout=stdout, stderr=stderr, timeout=85, preexec_fn=bound_output)
        if result.returncode:
            raise SystemExit(f'SSH failed ({result.returncode}); no baseline written')
        stdout.seek(0)
        report = sanitized(decode(stdout.read(1024*1024+1)), args.cohort)
    payload = json.dumps(report, indent=2, ensure_ascii=False).encode() + b'\n'
    fd = os.open(args.out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as f:
        f.write(payload)
    print(json.dumps({'output':str(args.out), 'sha256':hashlib.sha256(payload).hexdigest(),
                      'same_boot':report['same_boot'], 'bounded_elapsed':report['bounded_elapsed'],
                      'fota_off':report['fota_off']}))
    if not report['fota_off']:
        print('FOTA OFF could not be confirmed; inspect the two captured values. No settings changed.')

if __name__ == '__main__':
    main()
