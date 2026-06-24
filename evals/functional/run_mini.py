#!/usr/bin/env python3
"""Minimal functional end-to-end test runner for tiny SWE-bench-like tasks.

Runs the full raven-tui agent (via cargo) on a small workspace + bug report,
then verifies the fix by running the test.

Usage:
  python run_mini.py mini_add_one
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TUI_DIR = ROOT / "tui"
FUNCTIONAL_DIR = ROOT / "evals" / "functional"

def run_task(task_name: str, max_rounds: int = 10) -> bool:
    task_dir = FUNCTIONAL_DIR / task_name
    if not task_dir.exists():
        print(f"Task {task_name} not found in {FUNCTIONAL_DIR}")
        return False

    workspace = task_dir  # use in-place for simplicity; agent edits it
    prompt_file = task_dir / "problem_statement.txt"

    if not prompt_file.exists():
        print("Missing problem_statement.txt")
        return False

    # Ensure clean? For functional test we can copy to temp to not pollute source fixture.
    with tempfile.TemporaryDirectory() as tmp:
        ws = Path(tmp) / "ws"
        ws.mkdir()
        # copy the py files (not the problem)
        for f in task_dir.glob("*.py"):
            (ws / f.name).write_text(f.read_text())

        # write prompt
        (ws / "problem.txt").write_text(prompt_file.read_text())

        print(f"==> Running agent on {task_name} in {ws}")
        env = os.environ.copy()
        env["RAVEN_APPROVAL"] = "thunderdome"
        # No venv here; assume python in PATH for any exec the agent does.
        # For real runs they'd set RAVEN_EVAL_PYTHON etc.

        cmd = [
            "cargo", "run", "--release", "--bin", "raven-tui",
            "--manifest-path", str(TUI_DIR / "Cargo.toml"),
            "--",
            "--workspace", str(ws),
            "--fresh-session",
            "--prompt-file", str(ws / "problem.txt"),
            "--max-rounds", str(max_rounds),
            "--max-tokens", "4096",
        ]

        print("  $", " ".join(cmd))
        proc = subprocess.run(cmd, cwd=TUI_DIR, env=env, capture_output=True, text=True, timeout=120)
        print("Agent stdout tail:")
        print(proc.stdout[-2000:] if len(proc.stdout) > 2000 else proc.stdout)
        if proc.returncode != 0:
            print("Agent failed:", proc.stderr[-500:])
            return False

        # Now verify: run the test in the workspace
        print("==> Verifying fix by running test")
        verify_cmd = [sys.executable, "-m", "pytest", str(ws / "test_add_one.py"), "-q", "--tb=line"]
        vproc = subprocess.run(verify_cmd, cwd=ws, capture_output=True, text=True)
        print(vproc.stdout)
        print(vproc.stderr)
        success = vproc.returncode == 0
        print("Verify:", "PASSED" if success else "FAILED")
        return success

if __name__ == "__main__":
    task = sys.argv[1] if len(sys.argv) > 1 else "mini_add_one"
    ok = run_task(task)
    sys.exit(0 if ok else 1)
