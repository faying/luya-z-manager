#!/usr/bin/env python3
"""Compare historical A/B read-only captures; output never authorizes live operations."""
import argparse
import hashlib
import json
from pathlib import Path


def load(path, cohort):
    raw = path.read_bytes()
    if len(raw) > 1024*1024:
        raise ValueError('baseline exceeds 1 MiB')
    value = json.loads(raw)
    if (value.get('schema_version') != 1 or value.get('runtime_loadable') is not False or
            value.get('cohort') != cohort):
        raise ValueError(f'expected historical {cohort} capture')
    if value.get('same_boot') is not True or value.get('bounded_elapsed') is not True:
        raise ValueError('capture lacks same-boot/bounded-time evidence')
    return value, hashlib.sha256(raw).hexdigest()


def surface(report, key):
    entry = report.get('entries', {}).get(key, {})
    value = entry.get('value')
    if entry.get('exit_code') != 0 or not isinstance(value, dict):
        return 'unknown'
    expected = {'usb_methods':'zwrt_bsp.usb','typec_methods':'zwrt_bsp.typec',
                'powerbank_methods':'zwrt_bsp.powerbank'}[key]
    methods = value.get('methods')
    if value.get('object') != expected or not isinstance(methods, list) or not methods:
        return 'unknown'
    names = [m.get('name') for m in methods if isinstance(m, dict)]
    if len(names) != len(methods) or not all(isinstance(n,str) for n in names) or len(set(names)) != len(names):
        return 'unknown'
    return 'supported' if 'set' in names else 'unsupported'


def compare(a, b=None):
    def project(report):
        entries = report['entries']
        common = entries.get('common', {}).get('value') or {}
        return {'cohort':report['cohort'], 'full_external':common.get('integrate_version'),
                'internal':common.get('wa_inner_version'), 'model':common.get('model_name'),
                'hardware':common.get('hardware_version'),
                'setter_surface':{k:surface(report,k) for k in ('usb_methods','typec_methods','powerbank_methods')},
                'write_effect':'unknown', 'fota_off':report.get('fota_off')}
    return {'schema_version':1,'runtime_loadable':False,'A':project(a), 'B':project(b) if b else None,
            'status':'historical_comparison_only' if b else 'B_not_observed',
            'limitations':['Setter presence does not prove write support.',
                           'Identical external versions do not imply identical installation provenance.',
                           'No active slot, modem build, lifecycle epoch or first-install success inferred.',
                           'Native B baseline cannot be replaced by upgraded A.']}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--a',type=Path,required=True)
    p.add_argument('--b',type=Path)
    args=p.parse_args()
    a,ah=load(args.a,'upgraded')
    b,bh=load(args.b,'native') if args.b else (None,None)
    result=compare(a,b)
    result['artifact_sha256']={'A':ah,'B':bh}
    print(json.dumps(result,indent=2,ensure_ascii=False))

if __name__=='__main__': main()
