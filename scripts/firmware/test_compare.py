import importlib.util
from pathlib import Path
import unittest

spec=importlib.util.spec_from_file_location('compare',Path(__file__).with_name('compare.py'))
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)

class Compare(unittest.TestCase):
    def test_missing_b_and_surface_do_not_promote_writes(self):
        a={'cohort':'upgraded','entries':{'common':{'value':{'model_name':'MU5250','integrate_version':'CN_ZTE_MU5250V1.0.0B31'}},
            'usb_methods':{'exit_code':0,'value':{'object':'zwrt_bsp.usb','methods':[{'name':'list','signature':{}}]}}}}
        r=c.compare(a)
        self.assertEqual(r['status'],'B_not_observed');self.assertIsNone(r['B'])
        self.assertEqual(r['A']['setter_surface']['usb_methods'],'unsupported')
        self.assertEqual(r['A']['write_effect'],'unknown')
        self.assertFalse(r['runtime_loadable'])
        a['cohort']='native'
        b=c.compare({'cohort':'upgraded','entries':{}},a)
        self.assertEqual(b['B']['cohort'],'native')
        self.assertEqual(b['B']['write_effect'],'unknown')

    def test_incomplete_or_malformed_listing_is_unknown(self):
        for v in [None,{}, {'object':'zwrt_bsp.usb','methods':[]}, {'object':'zwrt_bsp.usb','methods':[{'name':'set'},{'name':'set'}]}]:
            r={'entries':{'usb_methods':{'exit_code':0,'value':v}}}
            self.assertEqual(c.surface(r,'usb_methods'),'unknown')

if __name__=='__main__':unittest.main()
