#!/usr/bin/env python3
"""Aggregate per-instance metrics.json into a run scorecard."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent


def _load(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError:
        return None


def _median(values: list[int | float]) -> float | None:
    if not values:
        return None
    return float(statistics.median(values))


def _rate(ok: int, total: int) -> float | None:
    if total == 0:
        return None
    return round(ok / total, 4)


def _load_probe(path: Path | None) -> dict[str, Any] | None:
    if not path or not path.is_file():
        return None
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError:
        return None


def aggregate(
    instance_ids: list[str],
    *,
    profile: str,
    mode: str,
    probe: dict[str, Any] | None = None,
) -> dict[str, Any]:
    per_instance: list[dict[str, Any]] = []
    failures = Counter()
    trajectory = Counter()
    loc_ok = 0
    repair_att = 0
    repair_ok = 0
    val_att = 0
    val_ok = 0

    resolved_count = 0
    durations: list[int] = []
    tool_calls: list[int] = []
    llm_rounds: list[int] = []
    tokens: list[int] = []

    for iid in instance_ids:
        mpath = ROOT / "results" / iid / "metrics.json"
        m = _load(mpath)
        if not m:
            per_instance.append({"instance_id": iid, "missing_metrics": True})
            failures["missing_metrics"] += 1
            continue

        per_instance.append(m)
        failures[m.get("failure_mode", "unknown")] += 1
        trajectory[m.get("harness", {}).get("trajectory_bucket", "unknown")] += 1

        swe = m.get("swebench") or {}
        if swe.get("resolved") is True:
            resolved_count += 1

        h = m.get("harness") or {}
        if (d := h.get("duration_ms")) is not None:
            durations.append(int(d))
        if (t := h.get("tool_calls")) is not None:
            tool_calls.append(int(t))
        if (r := h.get("llm_rounds")) is not None:
            llm_rounds.append(int(r))
        if (tok := h.get("total_tokens")) is not None and int(tok) > 0:
            tokens.append(int(tok))

        stages = m.get("stages") or {}
        if stages.get("localization_ok"):
            loc_ok += 1
        if stages.get("repair_attempted"):
            repair_att += 1
        if stages.get("repair_ok"):
            repair_ok += 1
        if stages.get("validation_attempted"):
            val_att += 1
        if stages.get("validation_ok"):
            val_ok += 1

    n = len(instance_ids)
    n_metrics = len([x for x in per_instance if not x.get("missing_metrics")])

    card: dict[str, Any] = {
        "version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "profile": profile,
        "mode": mode,
        "instances": instance_ids,
        "instance_count": n,
        "metrics_count": n_metrics,
        "swebench": {
            "resolved": resolved_count,
            "resolve_rate": _rate(resolved_count, n),
            "unresolved": n - resolved_count,
        },
        "harness": {
            "median_duration_ms": _median(durations),
            "median_tool_calls": _median(tool_calls),
            "median_llm_rounds": _median(llm_rounds),
            "median_total_tokens": _median(tokens),
            "total_duration_ms": sum(durations) if durations else 0,
            "total_tool_calls": sum(tool_calls) if tool_calls else 0,
            "total_tokens": sum(tokens) if tokens else 0,
        },
        "stages": {
            "localization_rate": _rate(loc_ok, n_metrics),
            "repair_attempt_rate": _rate(repair_att, n_metrics),
            "repair_success_rate": _rate(repair_ok, repair_att or n_metrics),
            "validation_attempt_rate": _rate(val_att, n_metrics),
            "validation_success_rate": _rate(val_ok, val_att or n_metrics),
            "end_to_end_resolve_rate": _rate(resolved_count, n_metrics),
        },
        "failure_modes": dict(failures),
        "trajectory_distribution": dict(trajectory),
        "per_instance": per_instance,
    }
    if probe:
        card["endpoint"] = {
            "base_url": probe.get("base_url"),
            "model_hint": probe.get("model_hint"),
            "model_id": probe.get("model_id"),
            "context_tokens": probe.get("context_tokens"),
            "matched_by": probe.get("matched_by"),
            "ready_for_agent": probe.get("ready_for_agent"),
        }
    return card


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "instance_ids",
        nargs="+",
        help="SWE-bench instance ids included in this run",
    )
    parser.add_argument("--profile", default="swebench-smoke")
    parser.add_argument("--mode", default="verify-grade")
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="scorecard.json output path",
    )
    parser.add_argument(
        "--probe-file",
        type=Path,
        help="probe.json from raven-eval run (endpoint metadata)",
    )
    args = parser.parse_args()

    card = aggregate(
        args.instance_ids,
        profile=args.profile,
        mode=args.mode,
        probe=_load_probe(args.probe_file),
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(card, indent=2))
    print(json.dumps(card, indent=2))


if __name__ == "__main__":
    main()