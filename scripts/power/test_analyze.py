"""Measurement integrity regressions; synthetic records, no device/network IO."""

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import analyze as a


BOOT = "12345678-aaaa-bbbb-cccc-123456789abc"


def minute(end=60_000, span=60_000):
    return {"meta": {"boot": BOOT, "mono_ms": end, "wall": 1_790_000_000},
            "span_ms": span, "ctxt": 600, "clones": 120, "wakes_total": 590,
            "exits": 140, "intr": 1000, "mw": 400, "exit_overflow": False,
            "ct_overflow": False, "trimmed": False, "charging": False, "screen_on_ms": 0,
            "self": {"cpu_ms": 1200, "ticks": 1, "collect_ms": 100, "wakes": 100},
            "procs": [{"name": "core", "cpu_ms": 1200, "wakes": 100}], "ifaces": []}


def hour():
    r = {"k": "hour_power", "boot": BOOT[:8], "from": 0, "to": 3600,
         "src": "counter", "e_partial": 0, "chg_s": 0, "off_home_cli_s": 0}
    for cell in a.CELLS:
        r[f"{cell}_s"] = 3600 if cell == "off_home" else 0
        r[f"{cell}_e"] = 400 if cell == "off_home" else 0
    return r


def quality_hour(observed=3540, energy=400_000):
    row = hour()
    row["counter_quality"] = {"v":1, "clock":"boottime_seconds", "samples":60,
        "pair_s":3540, "rejected_s":3540-observed, "sleep_observed_s":0,
        "rejected":{"baseline_only":1, "counter_missing":1} if observed < 3540 else {"baseline_only":1},
        "max_discharge_ua":600_000, "max_interval":{"from":60,"to":120,"awake_s":60,
            "q0_uah":20000,"q1_uah":10000,"v1_uv":4_000_000},
        "transition_check":"endpoints_only", "voltage_method":"interval_end",
        "jump_check":"requires_independently_validated_discharge_bound",
        "cells":{c:{"observed_s":observed if c == "off_home" else 0,
                    "uwh":energy if c == "off_home" else None} for c in a.CELLS}}
    if energy == 0:
        row["counter_quality"]["max_discharge_ua"] = 0
        row["counter_quality"]["max_interval"]["q1_uah"] = 20000
    return row


def activity_hour(start=0, end=3600, observed=3540, ticks=100, count=120):
    cpu = {"observed_s": observed, "ticks": ticks}
    return {"k": "hour_proc", "boot": BOOT[:8], "from": start, "to": end,
            "cpu_datad": 999,  # legacy value must never fill missing coverage
            "activity": {"v": 1, "clock": "boottime_seconds", "hz": 100,
                         "rejected": {"awake_gap": 1},
                         "core_cpu": cpu, "reaped_children_cpu": cpu,
                         "system_cpu": {"observed_s": observed, "busy_ticks": ticks, "total_ticks": 40000},
                         "task_creation": {"observed_s": observed, "count": count},
                         "context_switches": {"observed_s": observed, "count": count}}}


class Integrity(unittest.TestCase):
    def summary(self, rows, start=0, end=120_000):
        return a.minute_summary({"minutes": rows, "dropped_bytes": 0}, BOOT, start, end)

    def test_units_use_total_time_not_mean_of_rates(self):
        rows = [minute(), minute(180_000, 120_000)]
        result = self.summary(rows, end=180_000)
        self.assertEqual(result["metrics"]["clones"]["per_second"], 240/180)
        self.assertAlmostEqual(result["metrics"]["core_cpu_ms"]["single_core_percent"], 100*2400/180000)
        self.assertIsNone(result["true_wakeups"])
        self.assertIsNone(result["reaped_children_cpu_ms"])

    def test_wall_jump_does_not_change_result(self):
        rows = [minute(), minute(120_000)]
        before = self.summary(rows)
        rows[1]["meta"]["wall"] -= 8*3600
        self.assertEqual(before, self.summary(rows))

    def test_gap_crossing_boot_and_boundary_not_prorated(self):
        other = minute(180_000)
        other["meta"]["boot"] = "other"
        result = self.summary([minute(), minute(180_000), other], start=30_000, end=180_000)
        self.assertEqual(result["coverage"]["covered_ms"], 60_000)
        self.assertEqual(result["coverage"]["uncovered_ms"], 90_000)
        self.assertEqual(result["other_boot_records"], 1)
        self.assertEqual(result["outside_or_crossing_records"], 1)

    def test_overlap_and_duplicate_fail_instead_of_double_count(self):
        for rows in ([minute(), minute()], [minute(), minute(90_000)]):
            with self.assertRaisesRegex(ValueError, "overlap"):
                self.summary(rows)

    def test_missing_zero_and_overflow_are_different(self):
        rows = [minute(), minute(120_000)]
        rows[0]["clones"] = 0
        del rows[1]["clones"]
        rows[1]["exit_overflow"] = True
        rows[1]["trimmed"] = True
        result = self.summary(rows)
        self.assertEqual(result["metrics"]["clones"]["total"], 0)
        self.assertEqual(result["metrics"]["clones"]["observed_ms"], 60_000)
        self.assertEqual(result["quality"]["exit_overflow"], 1)
        self.assertEqual(result["quality"]["trimmed"], 1)
        self.assertIsNone(self.summary([])["metrics"]["clones"]["total"])

    def test_top_n_absence_not_zero_and_layers_not_combined(self):
        rows = [minute(), minute(120_000)]
        rows[1]["procs"] = []
        rows[0]["ifaces"] = [{"name": name, "rx_pkts": 10, "tx_pkts": 0,
                              "rx_bytes": 100, "tx_bytes": 0} for name in ("rmnet_data0", "tailscale0")]
        result = self.summary(rows)
        self.assertEqual(result["process_rows"][0]["present_ms"], 60_000)
        self.assertEqual(result["interface_deltas"]["rmnet_data0"]["rx_pkts"], 10)
        self.assertNotIn("total_packets", result)

    def test_counter_mean_is_energy_over_covered_time(self):
        result = a.hour_summary([hour()], BOOT, 0, 7_200_000, "off_home")
        self.assertEqual(result["candidate_counter_mwh"], 400)
        self.assertEqual(result["candidate_counter_mean_mw"], 400)
        self.assertEqual(result["candidate_coverage"]["coverage_fraction"], 0.5)
        self.assertEqual(result["status"], "needs_raw_counter_and_scenario_verification")

    def test_hour_fallback_charging_mixed_missing_rejected(self):
        changes = ({"src": "vi"}, {"e_partial": 1}, {"chg_s": 1},
                   {"on_home_s": 10}, {"off_home_s": 3590}, {"off_home_e": None},
                   {"off_home_cli_s": None}, {"off_home_cli_s": 60})
        for change in changes:
            with self.subTest(change=change):
                row = hour() | change
                result = a.hour_summary([row], BOOT, 0, 3_600_000, "off_home")
                self.assertIsNone(result["candidate_counter_mean_mw"])
                self.assertTrue(result["rejected"])

    def test_quality_uses_only_screened_energy_and_actual_seconds(self):
        row = quality_hour(3480, 400_000)
        row["off_home_e"] = 9999  # historical estimate must never fill a quality gap
        result = a.hour_summary([row], BOOT, 0, 7_200_000, "off_home")
        q = result["screened_counter"]
        self.assertIsNone(result["candidate_counter_mwh"])
        self.assertEqual(q["uwh"], 400_000)
        self.assertAlmostEqual(q["mean_mw"], 400_000*3.6/3480)
        self.assertAlmostEqual(q["coverage_fraction"], 3480/7200)
        self.assertEqual(q["producer_rejected_intervals"]["counter_missing"], 1)
        self.assertEqual(q["jump_check"], "unavailable_no_validated_bound")

    def test_quality_supplied_bound_rejects_entire_hour_not_prorated(self):
        row = quality_hour()
        result = a.hour_summary([row], BOOT, 0, 3_600_000, "off_home", 599_999)["screened_counter"]
        self.assertIsNone(result["uwh"])
        self.assertEqual(result["observed_s"], 0)
        self.assertEqual(result["excluded_records"][0]["reason"], "possible_counter_jump_exceeds_supplied_bound")
        self.assertEqual(result["excluded_records"][0]["raw_interval"]["q0_uah"], 20000)
        result = a.hour_summary([row], BOOT, 0, 3_600_000, "off_home", 600_000)["screened_counter"]
        self.assertEqual(result["uwh"], 400_000)
        self.assertEqual(result["jump_check"], "supplied_bound_only")

    def test_quality_unknown_schema_bad_totals_and_raw_peak_fail_closed(self):
        changes = ({"v":2}, {"v":True}, {"clock":"wall"}, {"samples":0}, {"pair_s":3601},
                   {"rejected_s":1}, {"sleep_observed_s":3541}, {"max_discharge_ua":1},
                   {"max_interval":None}, {"transition_check":"continuous"},
                   {"rejected":{"missing":True}}, {"cells":{}}, {"samples":True})
        for change in changes:
            with self.subTest(change=change):
                row = quality_hour()
                row["counter_quality"].update(change)
                result = a.hour_summary([row], BOOT, 0, 3_600_000, "off_home")["screened_counter"]
                self.assertIsNone(result["uwh"])
                self.assertEqual(result["excluded_records"][0]["reason"], "invalid_or_unknown_counter_quality")
        for value in (None, False, [], "unknown", 1):
            row = hour() | {"counter_quality":value}
            self.assertIsNone(a.hour_summary([row], BOOT, 0, 3_600_000, "off_home")["screened_counter"]["uwh"])

    def test_quality_legacy_missing_zero_and_weighted_subset_are_separate(self):
        old = a.hour_summary([hour()], BOOT, 0, 3_600_000, "off_home")["screened_counter"]
        self.assertEqual(old["status"], "unavailable")
        self.assertEqual(old["legacy_records_without_quality"], 1)
        first, second = quality_hour(1800, 0), quality_hour(3540, 200_000)
        second.update({"from":3600,"to":7200})
        second["counter_quality"]["max_interval"].update({"from":3660,"to":3720})
        result = a.hour_summary([first,second], BOOT, 0, 7_200_000, "off_home")["screened_counter"]
        self.assertEqual(result["observed_s"], 5340)
        self.assertAlmostEqual(result["mean_mw"], 200_000*3.6/5340)
        zero = a.hour_summary([first], BOOT, 0, 3_600_000, "off_home")["screened_counter"]
        self.assertEqual(zero["uwh"], 0)
        self.assertEqual(zero["mean_mw"], 0)

    def test_quality_bound_cli_rejects_unusable_limits(self):
        for bound in ("nan", "inf", "0", "-1"):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                a.main(["--hours","unused.jsonl","--boot",BOOT,"--from-ms","0","--to-ms","3600000",
                        "--max-discharge-ua",bound])
            self.assertEqual(error.exception.code, 2)

    def test_hour_duplicate_fails_and_different_boot_ignored(self):
        with self.assertRaisesRegex(ValueError, "overlap"):
            a.hour_summary([hour(), hour()], BOOT, 0, 3_600_000, "off_home")
        self.assertEqual(a.hour_summary([hour() | {"boot": "different"}], BOOT, 0,
                                       3_600_000, "off_home")["coverage"]["records"], 0)

    def test_legacy_zero_length_hours_outside_window_are_reported(self):
        # Shape reproduced from the live export: boot-start point rows precede
        # a complete hour. Keep fixture identities synthetic and raw exports private.
        rows = [hour() | {"from": 25200, "to": 28800},
                activity_hour(25200, 28800, observed=3522, ticks=7495, count=69367)]
        expected = a.hour_summary(rows, BOOT, 25200000, 28800000, "off_home")
        points = [{"k": kind, "boot": BOOT[:8], "from": point, "to": point}
                  for kind in ("hour_power", "hour_proc") for point in (47, 28800)]
        result = a.hour_summary(points + rows, BOOT, 25200000, 28800000, "off_home")
        self.assertEqual(result["coverage"], expected["coverage"])
        self.assertEqual(result["activity"]["metrics"], expected["activity"]["metrics"])
        self.assertEqual(result["candidate_counter_mean_mw"], expected["candidate_counter_mean_mw"])
        for section in (result, result["activity"]):
            self.assertEqual(section["outside_or_crossing_records"], 2)
            self.assertEqual(section["invalid_outside_window"], [
                {"from_ms": point*1000, "to_ms": point*1000,
                 "reason": "zero_length_outside_window"} for point in (47, 28800)])

    def test_zero_length_hours_inside_window_still_fail(self):
        for kind in ("hour_power", "hour_proc"):
            for point in (25200, 26000, 28799):
                with self.subTest(kind=kind, point=point), self.assertRaisesRegex(ValueError, "invalid interval"):
                    a.hour_summary([{"k": kind, "boot": BOOT[:8], "from": point, "to": point}],
                                   BOOT, 25200000, 28800000, "off_home")

    def test_unlocatable_or_reversed_hours_are_not_silently_excluded(self):
        for kind in ("hour_power", "hour_proc"):
            for bounds in ({"from":48,"to":47}, {"from":-1,"to":-1},
                           {"from":None,"to":47}, {"from":47},
                           {"from":True,"to":True}, {"from":"47","to":"47"}):
                with self.subTest(kind=kind, bounds=bounds), self.assertRaises((ValueError, KeyError, TypeError)):
                    a.hour_summary([{"k":kind,"boot":BOOT[:8]} | bounds],
                                   BOOT, 25200000, 28800000, "off_home")

    def test_cli_roundtrip_hash_and_invalid_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            ledger = Path(directory) / "ledger.json"
            ledger.write_text(json.dumps({"minutes": [minute()]}))
            args = ["--ledger", str(ledger), "--boot", BOOT, "--from-ms", "0", "--to-ms", "60000"]
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                a.main(args)
            result = json.loads(output.getvalue())
            self.assertEqual(len(result["inputs"]["ledger"]["sha256"]), 64)
            ledger.write_text('{"minutes": [')
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                a.main(args)
            self.assertEqual(error.exception.code, 2)

    def test_partial_jsonl_and_nonfinite_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "hours.jsonl"
            for text in ('{"k":"hour_power"}\n{"k":', '{"e":NaN}', '{"e":Infinity}'):
                path.write_text(text)
                with self.assertRaises(ValueError):
                    a.load(path, lines=True)

    def test_short_boot_and_invalid_records_fail_closed(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            a.main(["--hours", "unused.jsonl", "--boot", BOOT[:8], "--from-ms", "0", "--to-ms", "60000"])
        self.assertEqual(error.exception.code, 2)
        for data in ({"minutes": [None]}, {"minutes": [minute() | {"span_ms": True}]}):
            with self.assertRaises(ValueError):
                a.minute_summary(data, BOOT, 0, 60_000)
        with self.assertRaises(ValueError):
            a.hour_summary([None], BOOT, 0, 3_600_000, "off_home")

    def test_activity_uses_each_metric_observed_time_and_raw_ticks(self):
        first = activity_hour()
        second = activity_hour(3600, 7200, observed=1800, ticks=200, count=240)
        second["activity"]["core_cpu"]["cpu_s"] = 999  # recomputed from raw ticks/Hz
        result = a.hour_summary([hour(), first, second], BOOT, 0, 7_200_000, "off_home")["activity"]
        self.assertEqual(result["metrics"]["core_cpu"]["cpu_s"], 3)
        self.assertEqual(result["metrics"]["core_cpu"]["observed_s"], 5340)
        self.assertAlmostEqual(result["metrics"]["task_creation"]["per_second"], 360/5340)
        self.assertAlmostEqual(result["metrics"]["core_cpu"]["coverage_fraction"], 5340/7200)
        self.assertEqual(result["producer_rejected_intervals"]["awake_gap"], 2)

    def test_legacy_or_missing_activity_is_unknown_not_legacy_cpu_fill(self):
        result = a.hour_summary([hour(), {"k": "hour_proc", "boot": BOOT[:8], "from": 0,
                                          "to": 3600, "cpu_datad": 999}],
                                BOOT, 0, 3_600_000, "off_home")["activity"]
        self.assertIsNone(result["metrics"]["core_cpu"]["cpu_s"])
        self.assertEqual(result["activity_records"], 0)

    def test_activity_excess_coverage_unknown_schema_and_mixed_hz(self):
        bad = activity_hour(observed=4000)
        result = a.hour_activity_summary([bad], BOOT, 0, 3_600_000)
        self.assertIsNone(result["metrics"]["core_cpu"]["cpu_s"])
        self.assertTrue(result["invalid_metrics"])
        bad["activity"]["v"] = 99
        self.assertEqual(a.hour_activity_summary([bad], BOOT, 0, 3_600_000)["activity_records"], 0)
        second = activity_hour(3600, 7200)
        second["activity"]["hz"] = 1000
        with self.assertRaisesRegex(ValueError, "incompatible"):
            a.hour_activity_summary([activity_hour(), second], BOOT, 0, 7_200_000)


if __name__ == "__main__":
    unittest.main()
