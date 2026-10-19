import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('baseline', Path(__file__).with_name('baseline.py'))
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


class Baseline(unittest.TestCase):
    def sections(self):
        s = {k:(1,'') for k in b.COMMANDS}
        for k in ('boot_start','boot_end'):
            s[k]=(0,'12345678-1234-1234-1234-123456789abc\n')
        s['uptime_start']=(0,'10.01 1.0\n')
        s['uptime_end']=(0,'20.01 2.0\n')
        s['fota_mode']=(0,'0\n')
        s['fota_poll']=(0,'1\n')
        return s

    def test_redaction_and_separate_namespaces(self):
        s=self.sections()
        s['common']=(0,'{"model_name":"MU5250","integrate_version":"CN_ZTE_MU5250V1.0.0B31","wa_inner_version":"BD_other","imei":"private","imsi":"private"}')
        r=b.sanitized(s,'upgraded')
        self.assertTrue(r['same_boot'] and r['bounded_elapsed'] and r['fota_off'])
        self.assertNotIn('private',str(r))
        self.assertFalse(r['runtime_loadable'])
        self.assertEqual(r['entries']['common']['value']['wa_inner_version'],'BD_other')

    def test_mismatch_failure_and_unknown_fota_are_visible(self):
        for key,val in [('boot_end',(1,'')),('uptime_end',(0,'9 0')),('fota_mode',(0,'1'))]:
            s=self.sections();s[key]=val
            r=b.sanitized(s,'native')
            name={'boot_end':'same_boot','uptime_end':'bounded_elapsed','fota_mode':'fota_off'}[key]
            self.assertFalse(r[name])

    def test_complete_framing_and_no_partial_import(self):
        raw=''.join(f'LUYAZ_BEGIN {k}\n{v}\nLUYAZ_END {k} {rc}\n' for k,(rc,v) in self.sections().items())+'LUYAZ_COMPLETE\n'
        self.assertEqual(set(b.decode(raw.encode())),set(b.COMMANDS))
        for bad in [raw[:-4], 'garbage'+raw, raw.replace('LUYAZ_BEGIN common','LUYAZ_BEGIN board'),raw+'extra']:
            with self.assertRaises(ValueError):b.decode(bad.encode())
        with self.assertRaises(ValueError): b.decode(b'x'*(1024*1024+1))

    def test_methods_do_not_derive_write_support(self):
        s=self.sections()
        s['usb_methods']=(0,"'zwrt_bsp.usb' @abcd\n\t\"list\":{}\n")
        r=b.sanitized(s,'upgraded')
        self.assertEqual(r['entries']['usb_methods']['value'], {'object':'zwrt_bsp.usb','methods':[{'name':'list','signature':{}}]})
        for bad in ["'zwrt_bsp.usb' @abcd", "'zwrt_bsp.usb' @abcd\n\"set\":{}\n\"set\":{}", "'other' @abcd\n\"set\":{}"]:
            s['usb_methods']=(0,bad)
            self.assertIsNone(b.sanitized(s,'unknown')['entries']['usb_methods']['value'])

    def test_script_contains_no_mutation_or_at(self):
        for command in b.COMMANDS.values():
            self.assertNotIn(' set ',command)
            self.assertNotIn('AT+',command)
            self.assertNotIn('/dev/',command)

if __name__=='__main__': unittest.main()
