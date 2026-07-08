#!/usr/bin/env python3
"""Helpers for evals/run_patch_calib.sh — session id + seeded full_log.jsonl."""

from __future__ import annotations

import json
import sys
from pathlib import Path

FNV_OFFSET = 14695981039346656037
FNV_PRIME = 1099511628211


def fnv1a64(data: bytes) -> int:
    h = FNV_OFFSET
    for b in data:
        h ^= b
        h = (h * FNV_PRIME) & 0xFFFFFFFFFFFFFFFF
    return h


def make_session_id(workspace: Path, explicit_name: str | None) -> str:
    leaf = workspace.name.lower()
    leaf = "".join(c if c.isalnum() else "_" for c in leaf)
    name_part = ""
    if explicit_name:
        clean = "".join(
            c if c.isalnum() or c in "-_" else "-" for c in explicit_name
        ).strip("-")
        name_part = f"{clean}-"
    try:
        abs_path = str(workspace.resolve())
    except OSError:
        abs_path = str(workspace)
    short = f"{fnv1a64(abs_path.encode()):016x}"[:8]
    return f"{name_part}{leaf}-{short}"


def session_dir(home: Path, session_id: str) -> Path:
    return home / ".raven-hotel" / "sessions" / session_id


def write_jsonl(path: Path, entries: list[dict]) -> None:
    with path.open("w", encoding="utf-8") as f:
        for entry in entries:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def seed_meta(session_path: Path, sid: str, workspace: Path, summary: str) -> None:
    meta = {
        "session_id": sid,
        "workspace": str(workspace),
        "created_at": "2026-07-08T00:00:00Z",
        "updated_at": "2026-07-08T00:00:00Z",
        "recent_turns_summary": summary,
        "last_user_request": "Execute the approved plan step for single/ tarot TUI.",
    }
    session_path.mkdir(parents=True, exist_ok=True)
    (session_path / "meta.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")


def cargo_read_tool(cargo_rel: str, version: str) -> str:
    return (
        f"📄 /tmp/example/{cargo_rel} (8 lines)\n"
        f"   1 | [package]\n"
        f"   2 | name = \"tui-tarot\"\n"
        f"   3 | version = \"0.1.0\"\n"
        f"   4 | edition = \"2021\"\n"
        f"   5 | \n"
        f"   6 | [dependencies]\n"
        f'   7 | tui = "{version}"\n'
        f"   8 | crossterm = \"0.27\"\n"
    )


def warmup_entries() -> list[dict]:
    """Condensed prior-session noise similar to tui-f1f76f4c before the patch failure."""
    return [
        {"role": "user", "content": "hello", "has_tool_calls": False},
        {
            "role": "assistant",
            "content": "Hello! How can I help you today?",
            "has_tool_calls": False,
        },
        {
            "role": "user",
            "content": (
                "Execute the approved plan.\n"
                "Goal: Galaga-style C++ game\n"
                "Project root: Implement/\n"
                "Begin with step 1."
            ),
            "has_tool_calls": False,
        },
        {
            "role": "assistant",
            "content": "Created Implement/CMakeLists.txt for the Galaga project.",
            "has_tool_calls": False,
        },
        {
            "role": "user",
            "content": (
                "Execute step 1 of the tarot plan: create single/ Rust project "
                "with tui-rs, Cargo.toml, src/main.rs, then cargo check from single/."
            ),
            "has_tool_calls": False,
        },
        {
            "role": "assistant",
            "content": "Created single/Cargo.toml with tui 0.26 and single/src/main.rs.",
            "has_tool_calls": False,
        },
        {
            "role": "system",
            "content": "plan-execution nudge at step 1/6",
            "event": "nudge",
        },
        {
            "role": "user",
            "content": (
                "[Continue working. You paused to describe your plan instead of calling "
                "the next tool. Call the next tool now — do not narrate.]"
            ),
            "has_tool_calls": False,
        },
    ]


def noop_tail_entries(cargo_rel: str) -> list[dict]:
    """Ends with identical search/replace patch + no net change (plan-run failure)."""
    return [
        {
            "role": "user",
            "content": (
                "cargo check in single/ failed: crates.io has no tui 0.26. "
                "Fix the dependency version."
            ),
            "has_tool_calls": False,
        },
        {
            "role": "assistant",
            "content": (
                "The error shows tui 0.26 is not available. "
                "I need to update Cargo.toml to use a valid version."
            ),
            "has_tool_calls": False,
        },
        {
            "role": "assistant",
            "content": None,
            "has_tool_calls": True,
            "tool_calls": [
                {
                    "function": {
                        "name": "read",
                        "arguments": {"path": cargo_rel},
                    },
                    "id": "seed-read-1",
                }
            ],
        },
        {
            "role": "tool",
            "content": cargo_read_tool(cargo_rel, "0.26"),
            "tool_call_id": "seed-read-1",
            "has_tool_calls": False,
        },
        {
            "role": "assistant",
            "content": None,
            "has_tool_calls": True,
            "tool_calls": [
                {
                    "function": {
                        "name": "patch",
                        "arguments": {
                            "path": cargo_rel,
                            "search": 'tui = "0.26"',
                            "replace": 'tui = "0.26"',
                        },
                    },
                    "id": "seed-patch-noop",
                }
            ],
        },
        {
            "role": "tool",
            "content": (
                f"✅ Patched /tmp/example/{cargo_rel} "
                "(no net change — the content after the edit was identical to before)"
            ),
            "tool_call_id": "seed-patch-noop",
            "has_tool_calls": False,
        },
    ]


def cmd_session_id(workspace: str, session_name: str) -> None:
    print(make_session_id(Path(workspace), session_name))


def cmd_seed(mode: str, workspace: str, session_name: str, cargo_rel: str) -> None:
    ws = Path(workspace)
    home = Path.home()
    sid = make_session_id(ws, session_name)
    sp = session_dir(home, sid)

    if mode == "warmup":
        summary = (
            "Prior: Galaga plan in Implement/, rocket.py, wiki edits. "
            "Now on tarot TUI step 1 in single/."
        )
        entries = warmup_entries()
    elif mode == "noop":
        summary = (
            "Tarot step 1 in single/. cargo check failed on tui 0.26; "
            "last patch was a no-op (search == replace)."
        )
        entries = warmup_entries() + noop_tail_entries(cargo_rel)
    else:
        raise SystemExit(f"unknown seed mode: {mode}")

    seed_meta(sp, sid, ws, summary)
    write_jsonl(sp / "full_log.jsonl", entries)
    print(sid)


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit("usage: patch_calib_helpers.py <session-id|seed-warmup|seed-noop> ...")

    cmd = sys.argv[1]
    if cmd == "session-id":
        cmd_session_id(sys.argv[2], sys.argv[3])
    elif cmd in ("seed-warmup", "seed-noop"):
        mode = "warmup" if cmd == "seed-warmup" else "noop"
        cmd_seed(mode, sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        raise SystemExit(f"unknown command: {cmd}")


if __name__ == "__main__":
    main()