#!/usr/bin/env python3
"""Offline summaries of existing Luya-Z exports. Standard library; no device IO.

Input: GET /ledger JSON and/or long/hours.jsonl. Intervals are selected by
boot-relative time, never by wall clock. See docs/research/power-measurement.md.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
from uuid import UUID


CELLS = ("on_home", "on_abroad", "off_home", "off_abroad")
LIMITS = [
    "wakes means scheduler activity/context switches, not hardware or timer wakeups",
    "CPU rows are incomplete attribution, not a conserved whole-system CPU total",
    "self CPU covers the whole Core; collect_ms is elapsed collection time, not CPU time",
    "minute mw is a sample mean of V*I, not interval energy or runtime",
    "legacy interface records do not distinguish read failure from no counter deltas",
    "legacy hour counter energy has no interval quality; new screening sees endpoints only",
    "downward counter resets need an independently validated discharge bound; no default hardware limit",
    "no CPU-to-battery-runtime conversion; no automatic before/after causal claim",
]


def number(value):
    return type(value) in (int, float) and math.isfinite(value)


def nonnegative(value):
    return number(value) and value >= 0


def load(path, lines=False):
    raw = Path(path).read_bytes()
    # Reject non-standard JSON NaN/Infinity as well as truncated JSONL lines.
    def bad_constant(value):
        raise ValueError(f"non-finite JSON constant: {value}")
    parse = lambda text: json.loads(text, parse_constant=bad_constant)
    data = [parse(line) for line in raw.splitlines() if line.strip()] if lines else parse(raw)
    return data, {"sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}


def select(rows, bounds, start, end, invalid_outside=None):
    """Never prorate a crossing record, or silently deduplicate overlapping exports."""
    selected, excluded = [], 0
    for row in rows:
        a, b = bounds(row)
        # Old hourly producers emitted boot-start point rows. Only a finite,
        # nonnegative point provably outside [start, end) can be excluded safely.
        # Unknown/reversed bounds still fail, even when they appear historical.
        if (invalid_outside is not None and nonnegative(a) and number(b)
                and a == b and (a < start or a >= end)):
            invalid_outside.append({"from_ms": a, "to_ms": b,
                                    "reason": "zero_length_outside_window"})
            excluded += 1
            continue
        if not nonnegative(a) or not number(b) or b <= a:
            raise ValueError("invalid interval")
        if a >= start and b <= end:
            selected.append((a, b, row))
        else:
            excluded += 1
    selected.sort(key=lambda item: (item[0], item[1]))
    for previous, current in zip(selected, selected[1:]):
        if current[0] < previous[1]:
            raise ValueError("duplicate/overlapping intervals; use one export per window")
    return selected, excluded


def hour_bounds(row):
    a, b = row["from"], row["to"]
    if not nonnegative(a) or not nonnegative(b):
        raise ValueError("invalid interval")
    return a*1000, b*1000


def coverage(selected, start, end):
    covered = sum(b - a for a, b, _ in selected)
    return {"records": len(selected), "covered_ms": covered,
            "requested_ms": end - start, "coverage_fraction": covered / (end - start),
            "uncovered_ms": end - start - covered}


def measure(selected, getter, scale=1000):
    """A missing value contributes neither a zero nor denominator time."""
    total, duration, count = 0, 0, 0
    for a, b, row in selected:
        value = getter(row)
        if nonnegative(value):
            total += value
            duration += b - a
            count += 1
    return {"total": total if count else None, "observed_ms": duration,
            "records": count, "per_second": total * scale / duration if duration else None}


def minute_summary(data, boot, start, end):
    if not isinstance(data, dict) or not isinstance(data.get("minutes"), list):
        raise ValueError("ledger must be the GET /ledger object with a minutes array")
    if any(not isinstance(r, dict) or not isinstance(r.get("meta"), dict)
           for r in data["minutes"]):
        raise ValueError("invalid minute record or metadata")
    rows = [r for r in data["minutes"] if r["meta"]["boot"] == boot]
    if any(not number(r.get("span_ms")) or r["span_ms"] <= 0
           or not nonnegative(r["meta"].get("mono_ms")) for r in rows):
        raise ValueError("invalid minute time/span")
    selected, excluded = select(rows, lambda r: (
        r["meta"]["mono_ms"] - r["span_ms"], r["meta"]["mono_ms"]), start, end)
    totals = {}
    for key in ("clones", "ctxt", "intr", "exits", "wakes_total"):
        totals[key] = measure(selected, lambda r, k=key: r.get(k))
    core = measure(selected, lambda r: r.get("self", {}).get("cpu_ms"))
    core["single_core_percent"] = core["per_second"] / 10 if core["per_second"] is not None else None
    totals["core_cpu_ms"] = core
    totals["ledger_ticks"] = measure(selected, lambda r: r.get("self", {}).get("ticks"))
    totals["core_scheduler_activity"] = measure(selected, lambda r: r.get("self", {}).get("wakes"))
    totals["collection_elapsed_ms"] = measure(selected, lambda r: r.get("self", {}).get("collect_ms"))
    processes = {}
    for a, b, row in selected:
        for process in row.get("procs", []):
            processes.setdefault(process["name"], []).append((a, b, process))
    procs = []
    for name, entries in sorted(processes.items()):
        cpu = measure(entries, lambda r: r.get("cpu_ms"))
        procs.append({"name": name, "present_ms": sum(b-a for a, b, _ in entries),
                      "cpu_ms": cpu["total"],
                      "scheduler_activity": measure(entries, lambda r: r.get("wakes"))["total"],
                      "note": "top-N presence only; absence is unknown, not zero"})
    # Do not join layered interfaces, conntrack classes or the vendor packet fold.
    ifaces = {}
    for _, _, row in selected:
        for iface in row.get("ifaces", []):
            dest = ifaces.setdefault(iface["name"], {})
            for key in ("rx_pkts", "tx_pkts", "rx_bytes", "tx_bytes"):
                value = iface.get(key)
                if not nonnegative(value):
                    raise ValueError("invalid interface counter delta")
                dest[key] = dest.get(key, 0) + value
    mw = [r["mw"] for _, _, r in selected if number(r.get("mw"))]
    flags = {key: sum(r.get(key) is True for _, _, r in selected)
             for key in ("exit_overflow", "ct_overflow", "trimmed")}
    flags["taskstats_availability"] = "unknown: requires start/health evidence; zero exits is not proof"
    flags["dropped_bytes"] = data.get("dropped_bytes")
    flags["quality_fields_missing_records"] = sum(
        any(type(r.get(k)) is not bool for k in ("exit_overflow", "ct_overflow", "trimmed"))
        for _, _, r in selected)
    return {"coverage": coverage(selected, start, end), "outside_or_crossing_records": excluded,
            "other_boot_records": len(data["minutes"]) - len(rows), "quality": flags,
            "metrics": totals, "process_rows": procs, "interface_deltas": ifaces,
            "vi_mean_of_record_means_mw": sum(mw) / len(mw) if mw else None,
            "vi_record_count": len(mw), "true_wakeups": None, "reaped_children_cpu_ms": None,
            "polling_frequency": None, "write_bytes": None,
            "procs_other_cpu_ms": measure(selected, lambda r: r.get("procs_other", {}).get("cpu_ms")),
            "state": {"charging_records": sum(r.get("charging") is True for _, _, r in selected),
                      "screen_on_ms": measure(selected, lambda r: r.get("screen_on_ms"))["total"]}}


def hour_summary(data, boot, start, end, cell, max_discharge_ua=None):
    if not isinstance(data, list) or any(not isinstance(r, dict) for r in data):
        raise ValueError("hours must be JSONL records")
    # The resident format uses an eight-character boot ID. File provenance must
    # establish the full boot; a matching prefix alone is not device identity.
    boot_prefix = "".join(c for c in boot if c in "0123456789abcdefABCDEF")[:8]
    rows = [r for r in data if r.get("k") == "hour_power" and r.get("boot") == boot_prefix]
    invalid_outside = []
    selected, excluded = select(rows, hour_bounds, start, end, invalid_outside)
    candidates, rejected = [], []
    for a, b, row in selected:
        reasons = []
        # New producers must not fall back to their less strict historical fields.
        if "counter_quality" in row:
            reasons.append("historical_energy_requires_interval_quality")
        if row.get("src") != "counter":
            reasons.append("not_counter_source")
        if row.get("e_partial") != 0:
            reasons.append("partial_or_unknown_energy")
        if row.get("chg_s") != 0:
            reasons.append("charging_or_unknown")
        if row.get(f"{cell}_s") != (b-a)/1000 or any(
            row.get(f"{other}_s") != 0 for other in CELLS if other != cell
        ):
            reasons.append("mixed_or_incomplete_scenario")
        if not nonnegative(row.get(f"{cell}_e")):
            reasons.append("missing_or_invalid_energy")
        if cell == "off_home" and row.get("off_home_cli_s") != 0:
            reasons.append("clients_or_unknown")
        # Policy context is reported, not guessed from 'off' (which can mean
        # manual stop, idle pause or failure). Compare only using the manifest.
        if reasons:
            rejected.append({"from_ms": a, "to_ms": b, "reasons": reasons})
        else:
            candidates.append((a, b, row))
    duration = sum(b-a for a, b, _ in candidates)
    energy = sum(r[f"{cell}_e"] for _, _, r in candidates)
    return {"coverage": coverage(selected, start, end), "outside_or_crossing_records": excluded,
            "invalid_outside_window": invalid_outside,
            "cell": cell, "candidate_coverage": coverage(candidates, start, end),
            "candidate_counter_mwh": energy if candidates else None,
            "candidate_counter_mean_mw": energy * 3_600_000 / duration if duration else None,
            "status": "needs_raw_counter_and_scenario_verification",
            "screened_counter": counter_summary(selected, start, end, cell, max_discharge_ua),
            "activity": hour_activity_summary(data, boot, start, end),
            "boot_identity": "prefix_only; verify full boot and device in capture manifest",
            "rejected": rejected,
            "context": [{k: r.get(k) for k in ("from", "to", "chill_profile", "zone_tmax", "batt_tmax",
                        "batt_from", "batt_to", f"{cell}_quiet_pm", f"{cell}_wan_pkts")}
                        for _, _, r in selected]}


def counter_summary(selected, start, end, cell, max_discharge_ua=None):
    """Sum only producer-screened intervals; retain requested-window denominator.

    Bad intervals cannot be located from hourly totals. A bound failure therefore
    excludes the entire record, rather than inventing a corrected partial hour.
    """
    if max_discharge_ua is not None and (not number(max_discharge_ua) or max_discharge_ua <= 0):
        raise ValueError("max-discharge-ua must be a finite positive independently validated bound")
    observed, energy, available, missing = 0, 0, 0, 0
    rejected, producer_rejected = [], {}
    for lo, hi, row in selected:
        q = row.get("counter_quality")
        if "counter_quality" not in row:
            missing += 1
            continue
        span = (hi-lo)/1000
        valid = isinstance(q, dict) and type(q.get("v")) is int and q["v"] == 1 and q.get("clock") == "boottime_seconds"
        valid = valid and q.get("transition_check") == "endpoints_only" and q.get("voltage_method") == "interval_end"
        valid = valid and q.get("jump_check") == "requires_independently_validated_discharge_bound"
        valid = valid and type(q.get("samples")) is int and q["samples"] > 0
        fields = ("pair_s", "rejected_s", "sleep_observed_s")
        valid = valid and all(nonnegative(q.get(k)) and q[k] <= span for k in fields)
        cells = q.get("cells", {}) if isinstance(q, dict) else {}
        valid = valid and isinstance(cells, dict)
        seconds = 0
        if valid:
            for name in CELLS:
                c = cells.get(name)
                if not isinstance(c, dict) or not nonnegative(c.get("observed_s")):
                    valid = False
                    break
                n = c["observed_s"]
                if n > span or (n > 0 and not nonnegative(c.get("uwh"))) or (n == 0 and c.get("uwh") is not None):
                    valid = False
                    break
                seconds += n
        counts = q.get("rejected") if isinstance(q, dict) else None
        valid = valid and isinstance(counts, dict) and all(type(v) is int and v >= 0 for v in counts.values())
        if valid:
            valid = (seconds + q["rejected_s"] == q["pair_s"] and seconds <= span
                     and q["sleep_observed_s"] <= seconds)
        peak = None
        if valid and seconds > 0:
            raw = q.get("max_interval")
            valid = isinstance(raw, dict) and all(type(raw.get(k)) is int for k in
                        ("from", "to", "awake_s", "q0_uah", "q1_uah", "v1_uv"))
            if valid:
                dt = raw["to"]-raw["from"]
                valid = (lo/1000 <= raw["from"] < raw["to"] <= hi/1000 and
                         0 <= raw["awake_s"] <= min(dt, 120) and raw["v1_uv"] > 0 and
                         0 <= raw["q0_uah"]-raw["q1_uah"] <= 2**63-1 and
                         all(-2**63 <= raw[k] <= 2**63-1 for k in ("q0_uah", "q1_uah", "v1_uv")))
                if valid:
                    peak = (raw["q0_uah"]-raw["q1_uah"])*3600/dt
                    valid = nonnegative(q.get("max_discharge_ua")) and math.isclose(peak, q["max_discharge_ua"])
        if not valid:
            rejected.append({"from_ms":lo, "to_ms":hi, "reason":"invalid_or_unknown_counter_quality"})
            continue
        available += 1
        for reason, count in counts.items():
            producer_rejected[reason] = producer_rejected.get(reason, 0) + count
        if max_discharge_ua is not None and peak is not None and peak > max_discharge_ua:
            rejected.append({"from_ms":lo, "to_ms":hi, "reason":"possible_counter_jump_exceeds_supplied_bound",
                             "implied_discharge_ua":peak, "raw_interval":q["max_interval"]})
            continue
        c = cells[cell]
        observed += c["observed_s"]
        if c["observed_s"] > 0:
            energy += c["uwh"]
    return {"status":"screened_subset_still_needs_scenario_and_counter_verification" if available else "unavailable",
            "cell":cell, "quality_records":available, "legacy_records_without_quality":missing,
            "observed_s":observed, "requested_s":(end-start)/1000,
            "coverage_fraction":observed*1000/(end-start),
            "uwh":energy if observed else None, "mean_mw":energy*3.6/observed if observed else None,
            "max_discharge_bound_ua":max_discharge_ua,
            "jump_check":"supplied_bound_only" if max_discharge_ua is not None else "unavailable_no_validated_bound",
            "producer_rejected_intervals":producer_rejected, "excluded_records":rejected,
            "interval_locations":"not_reconstructable_from_hourly_totals",
            "transition_check":"endpoints_only"}


def hour_activity_summary(data, boot, start, end):
    """Fold new hour_proc counters using each metric's actual observed seconds.

    Legacy cpu_core is intentionally never used to fill these fields.
    """
    prefix = "".join(c for c in boot if c in "0123456789abcdefABCDEF")[:8]
    rows = [r for r in data if r.get("k") == "hour_proc" and r.get("boot") == prefix]
    invalid_outside = []
    selected, excluded = select(rows, hour_bounds, start, end, invalid_outside)
    definitions = {"core_cpu": "ticks", "reaped_children_cpu": "ticks",
                   "task_creation": "count", "context_switches": "count"}
    sums = {key: [0, 0] for key in (*definitions, "system_cpu")}
    total_ticks = 0
    hz_seen, rejected, invalid = set(), {}, []
    new_records = 0
    for a, b, row in selected:
        activity = row.get("activity")
        if not isinstance(activity, dict):
            continue
        if activity.get("v") != 1 or activity.get("clock") != "boottime_seconds":
            invalid.append({"from_ms": a, "reason": "unknown_activity_schema_or_clock"})
            continue
        hz = activity.get("hz")
        if not nonnegative(hz) or hz == 0:
            invalid.append({"from_ms": a, "reason": "unknown_tick_hz"})
            continue
        new_records += 1
        hz_seen.add(hz)
        reasons = activity.get("rejected", {})
        if not isinstance(reasons, dict):
            invalid.append({"from_ms": a, "reason": "invalid_activity_quality"})
            reasons = {}
        for reason, count in reasons.items():
            if nonnegative(count):
                rejected[reason] = rejected.get(reason, 0) + count
        for key, field in {**definitions, "system_cpu": "busy_ticks"}.items():
            metric = activity.get(key, {})
            if not isinstance(metric, dict):
                invalid.append({"from_ms": a, "reason": f"invalid_{key}"})
                continue
            seconds, value = metric.get("observed_s"), metric.get(field)
            if seconds == 0 and value is None:
                continue
            valid = nonnegative(seconds) and 0 < seconds <= (b-a)/1000 and nonnegative(value)
            if key == "system_cpu":
                total = metric.get("total_ticks")
                valid = valid and nonnegative(total) and total > 0 and value <= total
            if not valid:
                invalid.append({"from_ms": a, "reason": f"invalid_{key}_coverage_or_counter"})
                continue
            sums[key][0] += value
            sums[key][1] += seconds
            if key == "system_cpu":
                total_ticks += total
    if len(hz_seen) > 1:
        raise ValueError("incompatible activity tick Hz in one boot/window")
    hz = next(iter(hz_seen), None)
    metrics = {}
    for key, (value, seconds) in sums.items():
        result = {"observed_s": seconds,
                  "coverage_fraction": seconds*1000/(end-start)}
        if key in ("core_cpu", "reaped_children_cpu", "system_cpu"):
            result["ticks"] = value if seconds else None
            result["cpu_s"] = value/hz if seconds and hz else None
            if key == "system_cpu":
                result["total_ticks"] = total_ticks if seconds else None
                result["all_core_percent"] = 100*value/total_ticks if total_ticks else None
            else:
                result["single_core_percent"] = 100*value/hz/seconds if seconds and hz else None
        else:
            result["count"] = value if seconds else None
            result["per_second"] = value/seconds if seconds else None
        metrics[key] = result
    return {"status": "descriptive_only" if new_records else "unavailable_in_legacy_or_missing_records",
            "coverage": coverage(selected, start, end), "outside_or_crossing_records": excluded,
            "invalid_outside_window": invalid_outside,
            "activity_records": new_records, "hz": hz, "metrics": metrics,
            "producer_rejected_intervals": rejected, "invalid_metrics": invalid,
            "true_wakeups": None, "per_process_attribution": None}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", help="saved GET /ledger JSON; never /ledger/now partial")
    parser.add_argument("--hours", help="saved long/hours.jsonl")
    parser.add_argument("--boot", required=True, help="full boot ID from capture manifest")
    parser.add_argument("--from-ms", required=True, type=int, dest="start")
    parser.add_argument("--to-ms", required=True, type=int, dest="end")
    parser.add_argument("--cell", choices=CELLS, default="off_home")
    parser.add_argument("--max-discharge-ua", type=float,
                        help="independently validated maximum interval-average discharge current; no default")
    args = parser.parse_args(argv)
    if args.max_discharge_ua is not None and (not number(args.max_discharge_ua) or args.max_discharge_ua <= 0):
        parser.error("--max-discharge-ua requires a finite positive validated bound")
    if not (args.ledger or args.hours) or not args.boot or args.start < 0 or args.end <= args.start:
        parser.error("provide input, nonempty boot and 0 <= from-ms < to-ms")
    try:
        args.boot = str(UUID(args.boot))
    except ValueError:
        parser.error("--boot must be the full Linux boot UUID, not the hour record's prefix")
    out = {"schema": 1, "status": "descriptive_only_no_effectiveness_verdict",
           "boot": args.boot, "from_ms": args.start, "to_ms": args.end,
           "limitations": LIMITS, "inputs": {}}
    try:
        if args.ledger:
            data, provenance = load(args.ledger)
            out["inputs"]["ledger"] = provenance
            out["ledger"] = minute_summary(data, args.boot, args.start, args.end)
        if args.hours:
            data, provenance = load(args.hours, lines=True)
            out["inputs"]["hours"] = provenance
            out["hours"] = hour_summary(data, args.boot, args.start, args.end, args.cell, args.max_discharge_ua)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(2, f"invalid capture: {error}\n")
    print(json.dumps(out, ensure_ascii=False, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
