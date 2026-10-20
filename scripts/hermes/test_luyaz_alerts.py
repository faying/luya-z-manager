"""Tests for luyaz_alerts.py with a stand-in luyaz-mcp (fake_luyaz_mcp.py).

python3 -I -m unittest discover -s scripts/hermes -p 'test_*.py'
"""
import json
import os
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import luyaz_alerts as A  # noqa: E402


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def read_json(path):
    with open(path) as f:
        return json.load(f)


class World(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.world_path = os.path.join(self.dir, "world.json")
        self.log = os.path.join(self.dir, "calls.log")
        self.devices = os.path.join(self.dir, "devices")
        self.world = {"list": [], "devices": {}}

    def tearDown(self):
        self.tmp.cleanup()

    def with_list(self, on=True):
        # the script only checks that the list file exists; the fake answers list_devices
        if on:
            write(self.devices, "# luyaz-mcp reads this; the fake reads world.json\n")
        elif os.path.exists(self.devices):
            os.remove(self.devices)

    def run_once(self):
        write(self.world_path, json.dumps(self.world))
        env = dict(os.environ, FAKE_MCP_WORLD=self.world_path, FAKE_MCP_LOG=self.log)
        cfg = A.Cfg([sys.executable, "-I", os.path.join(HERE, "fake_luyaz_mcp.py")], self.devices, self.dir, env)
        if os.path.exists(cfg.last):
            os.remove(cfg.last)  # no night throttle in the tests
        return A.run(cfg)

    def seqs(self, out, who):
        return [int(line.split("#")[1].split()[0]) for line in out if line.startswith(f"🔔 {who} 告警")]

    def calls(self):
        with open(self.log) as f:
            got = [json.loads(x) for x in f]
        write(self.log, "")
        return got

    def legacy(self):
        return os.path.join(self.dir, "hermes-alerts.json")


class NoList(World):
    def test_single_device_as_before(self):
        self.world["devices"]["-"] = {"events": [1, 2, 3]}
        self.assertEqual(self.run_once(), [], "first run only sets the cursor")
        self.assertEqual(read_json(self.legacy())["next"], 3)
        self.world["devices"]["-"]["events"] = [1, 2, 3, 4, 5]
        self.world["devices"]["-"]["sms"] = [{"seq": 5, "kind": "k", "result": "failed"}]
        out = self.run_once()
        self.assertEqual(out[0], "🔔 U60 告警 #4 2026-10-20 08:00｜L：e4")
        self.assertEqual(self.seqs(out, "U60"), [4, 5])
        self.assertEqual(out[2], "📵 U60 告警短信没发出去（#5 k：failed）")
        self.assertEqual(self.run_once(), [], "nothing new, the same SMS not again")
        self.assertFalse(os.path.exists(os.path.join(self.dir, "luyaz-alerts.json")))

    def test_unreachable_is_quiet_and_catches_up(self):
        self.world["devices"]["-"] = {"events": [1]}
        self.run_once()
        self.world["devices"]["-"] = {"events": [1, 2], "unreachable": True}
        self.assertEqual(self.run_once(), [])
        self.world["devices"]["-"] = {"events": [1, 2]}
        self.assertEqual(self.seqs(self.run_once(), "U60"), [2])


class Several(World):
    def setUp(self):
        super().setUp()
        self.with_list()
        self.world["list"] = [{"name": "home", "enrolled": True, "device_id": "aaaaaaaaaaaa"},
                              {"name": "travel", "enrolled": False}]
        self.world["devices"] = {"home": {"device_id": "aaaaaaaaaaaa", "events": [1, 2]},
                                 "travel": {"device_id": "bbbbbbbbbbbb", "events": [7]}}
        self.assertEqual(self.run_once(), [])

    def add(self, name, *seqs):
        self.world["devices"][name]["events"] += list(seqs)

    def test_each_device_its_own_cursor_and_name(self):
        self.add("home", 3)
        self.add("travel", 8, 9)
        out = self.run_once()
        self.assertEqual(self.seqs(out, "[home]"), [3])
        self.assertEqual(self.seqs(out, "[travel]"), [8, 9])
        st = read_json(os.path.join(self.dir, "luyaz-alerts.json"))
        self.assertEqual({k: v["next"] for k, v in st["devices"].items()},
                         {"aaaaaaaaaaaa": 3, "bbbbbbbbbbbb": 9})

    def test_one_unreachable_others_go_on(self):
        self.add("home", 3)
        self.add("travel", 8)
        self.world["devices"]["home"]["unreachable"] = True
        out = self.run_once()
        self.assertEqual((self.seqs(out, "[home]"), self.seqs(out, "[travel]")), ([], [8]))
        del self.world["devices"]["home"]["unreachable"]
        out = self.run_once()
        self.assertEqual((self.seqs(out, "[home]"), self.seqs(out, "[travel]")), ([3], []))

    def test_rename_neither_repeats_nor_drops(self):
        self.add("home", 3)
        self.add("travel", 8)
        self.world["list"] = [{"name": "house", "enrolled": True, "device_id": "aaaaaaaaaaaa"},
                              {"name": "abroad", "enrolled": False}]
        self.world["devices"] = {"house": self.world["devices"].pop("home"),
                                 "abroad": self.world["devices"].pop("travel")}
        out = self.run_once()
        self.assertEqual((self.seqs(out, "[house]"), self.seqs(out, "[abroad]")), ([3], [8]))
        self.assertEqual(self.run_once(), [])

    def test_another_device_pauses_until_enrolled(self):
        # home's address now reaches another device: refused, said once, cursor kept
        self.world["devices"]["home"] = {"device_id": "cccccccccccc", "events": [1, 2, 50, 51]}
        out = self.run_once()
        self.assertEqual(len(out), 1)
        self.assertIn("[home] 现在回答的是另一台设备", out[0])
        self.assertIn("luyaz-mcp enroll home", out[0])
        self.assertEqual(self.run_once(), [], "said once")
        st = read_json(os.path.join(self.dir, "luyaz-alerts.json"))
        self.assertEqual(st["devices"]["aaaaaaaaaaaa"]["next"], 2, "old cursor kept")
        # the owner enrolls the new one: its alerts from now on, nothing replayed
        self.world["list"][0]["device_id"] = "cccccccccccc"
        out = self.run_once()
        self.assertEqual(self.seqs(out, "[home]"), [])
        self.assertTrue(any("现在是另一台设备" in x for x in out), out)
        self.add("home", 52)
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [52])

    def test_bad_list_stops_all_said_once(self):
        self.add("home", 3)
        self.world["list"] = {"error": "devices_file", "text": "The device list has a problem on line 2: bad."}
        out = self.run_once()
        self.assertEqual(len(out), 1)
        self.assertIn("设备清单有问题", out[0])
        self.assertIn("line 2", out[0])
        self.assertEqual(self.run_once(), [], "said once")
        self.world["list"] = [{"name": "home", "enrolled": True, "device_id": "aaaaaaaaaaaa"},
                              {"name": "travel", "enrolled": False}]
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [3], "fixed: goes on from the cursor")

    def test_pages_and_gap(self):
        self.add("travel", *range(8, 408))  # 400 new, more than 6 pages of 50
        self.calls()
        out = self.run_once()
        got = self.seqs(out, "[travel]")
        self.assertEqual((got[0], got[-1], len(got)), (8, 307, 300))
        self.assertEqual(sum(1 for c in self.calls() if c[1].get("device") == "travel"), 6)
        self.world["devices"]["travel"]["gap"] = True
        out = self.run_once()
        self.assertTrue(out[0].startswith("⚠️ [travel] 中间有告警已被设备清掉"), out[0])
        self.assertEqual(self.seqs(out, "[travel]")[-1], 407)


class Migration(World):
    """D7: the old single-device cursor goes to the device only when it is the one enrolled."""

    def setUp(self):
        super().setUp()
        write(self.legacy(), json.dumps({"next": 27, "sms": [[26, "failed"]]}))
        self.world["devices"]["home"] = {"device_id": "aaaaaaaaaaaa", "events": list(range(1, 31)),
                                         "sms": [{"seq": 26, "kind": "k", "result": "failed"}]}

    def test_critical_no_repeat_no_miss(self):
        self.with_list()
        self.world["list"] = [{"name": "home", "enrolled": True, "device_id": "aaaaaaaaaaaa"}]
        out = self.run_once()
        self.assertEqual(self.seqs(out, "[home]"), [28, 29, 30])
        self.assertFalse(any("短信" in x for x in out), "the SMS already reported stays reported")
        self.assertFalse(os.path.exists(self.legacy()))
        self.assertTrue(os.path.exists(self.legacy() + ".migrated"))
        self.world["devices"]["home"]["events"].append(31)
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [31])

    def test_not_enrolled_old_file_left_alone(self):
        self.with_list()
        self.world["list"] = [{"name": "home", "enrolled": False}]
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [])
        self.assertEqual(read_json(self.legacy())["next"], 27, "untouched")
        self.world["devices"]["home"]["events"].append(31)
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [31])

    def test_two_devices_old_file_left_alone(self):
        self.with_list()
        self.world["list"] = [{"name": "home", "enrolled": True, "device_id": "aaaaaaaaaaaa"},
                              {"name": "travel", "enrolled": False}]
        self.world["devices"]["travel"] = {"device_id": "bbbbbbbbbbbb", "events": [5]}
        self.assertEqual(self.run_once(), [])
        self.assertTrue(os.path.exists(self.legacy()))

    def test_old_admin_web_keeps_the_old_file(self):
        self.with_list()
        self.world["list"] = [{"name": "home", "enrolled": False}]
        del self.world["devices"]["home"]["device_id"]
        self.assertEqual(self.seqs(self.run_once(), "[home]"), [28, 29, 30])
        self.assertEqual(read_json(self.legacy())["next"], 30)


class Night(unittest.TestCase):
    def test_night_asks_at_most_every_half_hour(self):
        with tempfile.NamedTemporaryFile() as f:
            now = time.time()
            os.utime(f.name, (now - 60, now - 60))
            self.assertTrue(A.skip_tick(f.name, hour=2, now=now))
            self.assertTrue(A.skip_tick(f.name, hour=23, now=now))
            self.assertFalse(A.skip_tick(f.name, hour=12, now=now), "daytime: every tick")
            self.assertFalse(A.skip_tick(f.name, hour=2, now=now + 31 * 60))
        self.assertFalse(A.skip_tick("/nonexistent/last", hour=2))


if __name__ == "__main__":
    unittest.main()
