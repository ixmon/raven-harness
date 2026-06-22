#!/usr/bin/env python3
"""Export SWE-bench Lite dev instances to evals/swebench/instances/<id>.json."""

from __future__ import annotations

import json
from pathlib import Path

import pyarrow.parquet as pq
from huggingface_hub import hf_hub_download

ROOT = Path(__file__).resolve().parent
INSTANCES_DIR = ROOT / "instances"
MANIFEST = ROOT / "instances.json"


def load_dev_table():
    path = hf_hub_download(
        "princeton-nlp/SWE-bench_Lite",
        "data/dev-00000-of-00001.parquet",
        repo_type="dataset",
    )
    return {row["instance_id"]: row for row in pq.read_table(path).to_pylist()}


def main() -> None:
    manifest = json.loads(MANIFEST.read_text())
    ids: list[str] = manifest.get("smoke_trio", [])
    if not ids:
        raise SystemExit("instances.json smoke_trio is empty")

    table = load_dev_table()
    INSTANCES_DIR.mkdir(parents=True, exist_ok=True)

    for iid in ids:
        if iid not in table:
            raise SystemExit(f"instance not in dev split: {iid}")
        out = INSTANCES_DIR / f"{iid}.json"
        out.write_text(json.dumps(table[iid], indent=2))
        print(f"wrote {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()