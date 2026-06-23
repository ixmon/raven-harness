#!/usr/bin/env python3
"""Merge SWE-bench grade + Raven trajectory into per-instance metrics.json."""

from __future__ import annotations

import argparse
import json
import re
import statistics
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent

LOCALIZATION_TOOLS = frozenset({"read", "grep", "find", "list", "retrieve_session", "read_summary"})
REPAIR_TOOLS = frozenset({"write", "patch"})
VALIDATION_TOOLS = frozenset({"exec"})

TRAJECTORY_BUCKETS = (
    ("none", 0, 0),
    ("short", 1, 4),
    ("medium", 5, 14),
    ("long", 15, 29),
    ("very_long", 30, 10_000),
)


def _load_json(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError:
        return None


def _read_log_lines(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    rows: list[dict[str, Any]] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return rows


def _trajectory_bucket(tool_calls: int) -> str:
    for name, lo, hi in TRAJECTORY_BUCKETS:
        if lo <= tool_calls <= hi:
            return name
    return "very_long"


def _tool_sequence(rows: list[dict[str, Any]]) -> list[str]:
    seq: list[str] = []
    for row in rows:
        if row.get("role") != "assistant":
            continue
        for name in row.get("tool_names") or []:
            if isinstance(name, str):
                seq.append(name)
    return seq


def _stage_flags(tool_names: set[str], exec_bodies: list[str]) -> dict[str, bool]:
    ran_exec = bool(tool_names & VALIDATION_TOOLS)
    validation_signals = any(
        re.search(r"pytest|unittest|npm test|cargo test|tox", body, re.I)
        for body in exec_bodies
    )
    return {
        "localization": bool(tool_names & LOCALIZATION_TOOLS),
        "repair": bool(tool_names & REPAIR_TOOLS),
        "validation": ran_exec and validation_signals,
    }


def _failure_mode(
    *,
    resolved: bool | None,
    patch_bytes: int,
    patch_applied: bool | None,
    round_limit_hit: bool,
    tool_calls: int,
    repair_attempted: bool,
    agent_exit_code: int | None,
) -> str:
    if agent_exit_code not in (None, 0):
        return "agent_error"
    if tool_calls == 0:
        return "no_tools"
    if not repair_attempted:
        return "analysis_only"
    if patch_bytes == 0:
        return "empty_patch"
    if patch_applied is False:
        return "patch_apply_failed"
    if resolved is True:
        return "resolved"
    if round_limit_hit:
        return "round_limit"
    if resolved is False:
        return "tests_failed"
    return "unresolved"


def extract_instance_metrics(result_dir: Path, *, mode: str = "full") -> dict[str, Any]:
    report = _load_json(result_dir / "report.json") or {}
    turn = _load_json(result_dir / "harness_turn.json") or {}
    timing = _load_json(result_dir / "run_timing.json") or {}
    meta = _load_json(result_dir / "meta.json") or {}
    instance = _load_json(result_dir / "instance.json") or {}

    log_rows = _read_log_lines(result_dir / "raven_log.jsonl")
    tool_seq = _tool_sequence(log_rows)
    tool_names = set(tool_seq)

    exec_bodies = [
        str(r.get("content") or "")
        for r in log_rows
        if r.get("role") == "tool" and r.get("tool_call_id")
    ]

    patch_path = result_dir / "model_patch.diff"
    patch_bytes = patch_path.stat().st_size if patch_path.is_file() else 0

    tool_calls = int(turn.get("tool_calls") or len(tool_seq))
    llm_rounds = int(turn.get("llm_rounds") or 0)
    duration_ms = int(
        turn.get("duration_ms")
        or timing.get("wall_duration_ms")
        or 0
    )

    stages = _stage_flags(tool_names, exec_bodies)
    resolved = report.get("resolved")
    patch_applied = report.get("patch_successfully_applied")
    round_limit_hit = bool(turn.get("round_limit_hit"))
    if not round_limit_hit:
        round_limit_hit = any(
            "tool round limit" in str(r.get("content") or "").lower()
            for r in log_rows
            if r.get("role") == "assistant"
        )

    failure_mode = _failure_mode(
        resolved=resolved if isinstance(resolved, bool) else None,
        patch_bytes=patch_bytes,
        patch_applied=patch_applied if isinstance(patch_applied, bool) else None,
        round_limit_hit=round_limit_hit,
        tool_calls=tool_calls,
        repair_attempted=stages["repair"],
        agent_exit_code=timing.get("raven_exit_code"),
    )

    assistant_turns = sum(1 for r in log_rows if r.get("role") == "assistant")
    tool_messages = sum(1 for r in log_rows if r.get("role") == "tool")

    return {
        "version": 1,
        "instance_id": report.get("instance_id")
        or instance.get("instance_id")
        or result_dir.name,
        "mode": mode,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "swebench": {
            "resolved": resolved,
            "resolved_status": report.get("resolved_status"),
            "patch_successfully_applied": patch_applied,
            "patch_bytes": patch_bytes,
            "tests_status": report.get("tests_status"),
        },
        "harness": {
            "model": turn.get("model") or meta.get("model"),
            "duration_ms": duration_ms,
            "llm_rounds": llm_rounds,
            "assistant_turns": assistant_turns,
            "tool_messages": tool_messages,
            "tool_calls": tool_calls,
            "trajectory_bucket": _trajectory_bucket(tool_calls),
            "round_limit_hit": round_limit_hit,
            "prompt_tokens": int(turn.get("prompt_tokens") or 0),
            "completion_tokens": int(turn.get("completion_tokens") or 0),
            "total_tokens": int(turn.get("total_tokens") or 0),
            "tool_counts": turn.get("tool_counts") or {},
            "tool_sequence": tool_seq,
        },
        "stages": {
            **stages,
            "localization_ok": stages["localization"],
            "repair_attempted": stages["repair"],
            "repair_ok": stages["repair"] and patch_bytes > 0,
            "validation_attempted": stages["validation"],
            "validation_ok": resolved is True,
        },
        "failure_mode": failure_mode,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "result_dir",
        type=Path,
        help="evals/swebench/results/<instance_id>/",
    )
    parser.add_argument(
        "--mode",
        default="full",
        help="verify-grade or full",
    )
    parser.add_argument(
        "--out",
        type=Path,
        help="default: <result_dir>/metrics.json",
    )
    args = parser.parse_args()

    metrics = extract_instance_metrics(args.result_dir.resolve(), mode=args.mode)
    out = args.out or (args.result_dir / "metrics.json")
    out.write_text(json.dumps(metrics, indent=2))
    print(json.dumps(metrics, indent=2))


if __name__ == "__main__":
    main()