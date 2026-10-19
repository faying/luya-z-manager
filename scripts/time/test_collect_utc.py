import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('collect_utc', Path(__file__).with_name('collect_utc.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def report():
    tracking = '''Reference ID : CB007101 (203.0.113.1)
Stratum : 2
Ref time (UTC) : Thu Sep 24 14:13:20 2026
System time : 0.001 seconds slow of NTP time
Root delay : 0.020 seconds
Root dispersion : 0.002 seconds
Leap status : Normal'''
    # UTC 2026-09-24 14:13:20, independent of test host wall clock.
    stamp = m.dt.datetime(2026, 9, 24, 14, 13, 20, tzinfo=m.dt.timezone.utc).timestamp()
    return dict(start=dict(wall=stamp, mono=10), end=dict(wall=stamp+0.25, mono=10.25),
                tracking_before=tracking, tracking_after=tracking,
                sources_before='^* 203.0.113.1 2 6 377 1 +0ns[+0ns] +/- 10ms',
                sources_after='^* 203.0.113.1 2 6 377 1 +0ns[+0ns] +/- 10ms',
                authdata='203.0.113.1 NTS 1 15 256 60 0 0 8 100',
                ntpdata='Remote address : 203.0.113.1 (CB007101)\nLeap status : Normal\nAuthenticated : Yes\nNTP tests : 111 111 1111',
                device=f'abcdef01-2345\n100.00 0\n{int(stamp)+28800}\n8.00\n{int(stamp)+28800}\nabcdef01-2345\n100.10 0\n')


class EvidenceTests(unittest.TestCase):
    def test_valid_bounded_attestation(self):
        a = m.build_anchor(report())
        self.assertEqual(a['source'], 'chrony_nts')
        self.assertEqual(a['uncertainty_s'], 3)
        self.assertEqual(a['observed_at']['seconds'], 100)
        self.assertEqual(len(a['evidence_sha256']), 64)
        self.assertEqual(a['valid_for_s'], 900)

    def test_missing_unauthenticated_stale_and_wrong_source_fail(self):
        mutations = [
            ('authdata', '203.0.113.1 - 0 0'),
            ('authdata', '203.0.113.2 NTS 1'),
            ('ntpdata', 'Authenticated : No'),
            ('ntpdata', report()['ntpdata'].replace('111 111 1111', '111 111 0111')),
            ('tracking_before', report()['tracking_before'].replace('Normal', 'Not synchronised')),
            ('tracking_before', report()['tracking_before'].replace('Sep 24', 'Sep 23')),
            ('tracking_before', report()['tracking_before'].replace('0.002', 'NaN')),
            ('tracking_before', report()['tracking_before'].replace('0.001', '5.0')),
            ('sources_after', '^* 203.0.113.2 2 6 377 1'),
        ]
        for key, value in mutations:
            with self.subTest(key=key, value=value):
                r = report(); r[key] = value
                with self.assertRaises(m.Unverified): m.build_anchor(r)

    def test_clock_steps_unknown_boot_model_and_slow_acquisition_fail(self):
        for kind in range(7):
            r = copy.deepcopy(report())
            if kind == 0: r['end']['wall'] += 28800
            if kind == 1: r['end']['mono'] += 11
            if kind == 2: r['end']['mono'] += 3; r['end']['wall'] += 3
            if kind == 3: r['device'] = r['device'].replace('abcdef01-2345\n100.10', 'abcdef02-2345\n100.10')
            if kind == 4: r['device'] = r['device'].replace('8.00', '5.00')
            if kind == 5: r['device'] = r['device'].replace('100.10', '0.10')
            if kind == 6: r['device'] = r['device'].replace('100.10', 'nan')
            with self.subTest(kind=kind):
                with self.assertRaises(m.Unverified): m.build_anchor(r)


if __name__ == '__main__': unittest.main()
