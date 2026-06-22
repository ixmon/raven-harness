#!/usr/bin/env python3
"""Lightweight local SWE-bench grading (no Docker)."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from swebench.harness.constants import (
    END_TEST_OUTPUT,
    KEY_INSTANCE_ID,
    KEY_PREDICTION,
    START_TEST_OUTPUT,
    MAP_REPO_VERSION_TO_SPECS,
)
from swebench.harness.grading import get_eval_report
from swebench.harness.test_spec.python import get_requirements, get_test_directives
from swebench.harness.test_spec.test_spec import make_test_spec

ROOT = Path(__file__).resolve().parent


def _run(cmd: list[str], *, cwd: Path, env: dict | None = None) -> subprocess.CompletedProcess:
    merged = os.environ.copy()
    if env:
        merged.update(env)
    return subprocess.run(
        cmd,
        cwd=cwd,
        env=merged,
        text=True,
        capture_output=True,
        check=False,
    )


def _pick_python(spec_python: str) -> str:
    import shutil

    candidates = [
        f"python{spec_python}",
        "python3.9",
        "python3.10",
        "python3.11",
        "python3",
    ]
    for candidate in candidates:
        path = shutil.which(candidate)
        if not path:
            continue
        proc = subprocess.run(
            [path, "--version"],
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode == 0:
            return path
    raise RuntimeError(f"no Python interpreter found (wanted {spec_python})")


def _reset_repo(repo: Path, base_commit: str) -> None:
    proc = _run(["git", "reset", "--hard", base_commit], cwd=repo)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or proc.stdout)
    _run(["git", "clean", "-fdx"], cwd=repo)


def _apply_patch(repo: Path, patch_text: str, label: str) -> None:
    if not patch_text.strip():
        raise RuntimeError(f"{label} patch is empty")
    proc = subprocess.run(
        ["git", "apply", "--verbose", "-"],
        cwd=repo,
        input=patch_text,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"failed to apply {label} patch:\n{proc.stdout}\n{proc.stderr}"
        )


def _create_venv(repo: Path, spec_python: str) -> Path:
    """Create an isolated venv using uv (preferred) or system python."""
    import shutil

    venv_dir = repo / ".swebench-venv"
    if venv_dir.exists():
        shutil.rmtree(venv_dir)

    py_spec = f"python{spec_python}"
    uv = shutil.which("uv")
    if uv:
        install = _run([uv, "python", "install", py_spec], cwd=repo)
        if install.returncode != 0:
            raise RuntimeError(
                f"uv python install {py_spec} failed:\n{install.stderr or install.stdout}"
            )
        proc = _run([uv, "venv", "--python", py_spec, str(venv_dir)], cwd=repo)
        if proc.returncode == 0:
            return venv_dir
        raise RuntimeError(proc.stderr or proc.stdout)

    py = _pick_python(spec_python)
    proc = _run([py, "-m", "venv", str(venv_dir)], cwd=repo)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or proc.stdout)
    return venv_dir


def _setup_venv(repo: Path, instance: dict, specs: dict) -> Path:
    venv_dir = _create_venv(repo, str(specs.get("python", "3.9")))
    python = venv_dir / "bin" / "python"

    def pip_install(*args: str) -> None:
        proc = _run([str(python), "-m", "pip", "install", *args], cwd=repo)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr or proc.stdout)

    proc = _run([str(python), "-m", "ensurepip", "--upgrade"], cwd=repo)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or proc.stdout)
    # setuptools 82+ drops pkg_resources; many SWE-bench repos still import it.
    pip_install("-U", "pip", "setuptools<81", "wheel", "pytest-mock")

    if instance.get("repo") == "pvlib/pvlib-python":
        pip_install("numpy<2")

    for pkg in specs.get("pip_packages", []) or []:
        pip_install(pkg)

    packages = specs.get("packages")
    if packages == "requirements.txt":
        reqs_path = repo / ".swebench-requirements.txt"
        reqs_path.write_text(get_requirements(instance))
        pip_install("-r", str(reqs_path))
    elif packages and str(packages).endswith(".txt"):
        pip_install("-r", str(packages))
    elif packages:
        pip_install(*str(packages).split())

    install = specs.get("install", "python -m pip install -e .")
    if "pip install" in install and "-e" in install:
        extra = ".[dev]" if ".[dev]" in install else "."
        pip_install("-e", extra)
    elif install.strip().startswith("python "):
        proc = _run(
            [str(python), *install.strip().split()[1:]],
            cwd=repo,
        )
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr or proc.stdout)
    else:
        proc = _run(install.split(), cwd=repo)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr or proc.stdout)

    return python


def _pytest_targets(instance: dict) -> list[str]:
    """Match SWE-bench harness: pytest the files touched by test_patch."""
    return get_test_directives(instance)


def grade_instance(
    instance: dict,
    repo_dir: Path,
    model_patch: str,
    *,
    work_dir: Path | None = None,
) -> dict:
    work = work_dir or (ROOT / "results" / instance["instance_id"])
    work.mkdir(parents=True, exist_ok=True)

    base_commit = instance["base_commit"]
    test_patch = instance.get("test_patch", "")
    specs = MAP_REPO_VERSION_TO_SPECS[instance["repo"]][instance["version"]]
    test_spec = make_test_spec(instance)

    _reset_repo(repo_dir, base_commit)
    _apply_patch(repo_dir, test_patch, "test")
    _apply_patch(repo_dir, model_patch, "model")

    python = _setup_venv(repo_dir, instance, specs)
    targets = _pytest_targets(instance)
    if not targets:
        raise RuntimeError("no pytest targets in instance")

    proc = _run([str(python), "-m", "pytest", "-rA", *targets], cwd=repo_dir)
    test_log = "\n".join(
        [
            START_TEST_OUTPUT,
            proc.stdout,
            proc.stderr,
            END_TEST_OUTPUT,
        ]
    )
    test_log_path = work / "test_output.log"
    test_log_path.write_text(test_log)

    prediction = {
        KEY_INSTANCE_ID: instance["instance_id"],
        "model_name_or_path": "raven-harness",
        KEY_PREDICTION: model_patch,
    }
    report_map = get_eval_report(
        test_spec,
        prediction,
        str(test_log_path),
        include_tests_status=True,
    )
    entry = report_map[instance["instance_id"]]
    entry["pytest_exit_code"] = proc.returncode
    entry["resolved_status"] = "resolved" if entry.get("resolved") else "unresolved"
    return entry


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("instance_id")
    parser.add_argument("--repo", type=Path, required=True, help="Checked-out repo workspace")
    parser.add_argument(
        "--patch",
        type=Path,
        required=True,
        help="Model patch file (unified diff)",
    )
    parser.add_argument(
        "--instance-json",
        type=Path,
        help="Instance JSON (default: instances/<id>.json)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        help="Write report.json here (default: results/<id>/report.json)",
    )
    args = parser.parse_args()

    instance_path = args.instance_json or (ROOT / "instances" / f"{args.instance_id}.json")
    if not instance_path.is_file():
        raise SystemExit(
            f"missing {instance_path} — run export_instances.py first"
        )

    instance = json.loads(instance_path.read_text())
    model_patch = args.patch.read_text()
    out_dir = args.out.parent if args.out else (ROOT / "results" / args.instance_id)
    report = grade_instance(instance, args.repo.resolve(), model_patch, work_dir=out_dir)

    out_path = args.out or (out_dir / "report.json")
    out_path.write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
    if not report.get("resolved"):
        sys.exit(1)


if __name__ == "__main__":
    main()