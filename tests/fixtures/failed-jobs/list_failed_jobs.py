#!/usr/bin/env python3
"""Sample workflow script that reads fake_jobs_cli.py and filters status=failed."""

import argparse
import json
from pathlib import Path
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser(description="Filter failed jobs from fake_jobs_cli.py")
    parser.add_argument(
        "--mode",
        choices=["v1", "v1-extra", "v2-drift", "nonzero"],
        default="v1",
        help="Mode forwarded to fake_jobs_cli.py",
    )
    parser.add_argument(
        "--extra-fields",
        action="store_true",
        help="Include extra unrelated fields in the JSON output",
    )
    args = parser.parse_args()

    cli_path = Path(__file__).resolve().with_name("fake_jobs_cli.py")
    proc = subprocess.run(
        [sys.executable, str(cli_path), "--mode", args.mode],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        if proc.stderr:
            sys.stderr.write(proc.stderr)
        return proc.returncode

    payload = json.loads(proc.stdout)
    jobs = payload.get("jobs", [])
    # Intentionally reads "status" so upstream drift to "state" yields a false empty list.
    failed = [job for job in jobs if job.get("status") == "failed"]

    result = {
        "api_version": payload.get("api_version"),
        "failed_count": len(failed),
        "failed_job_ids": [job["id"] for job in failed],
        "failed_jobs": failed,
        "last_error": None,
        "meta/schema": payload.get("api_version"),
        "meta~source": "fake_jobs_cli",
    }
    if args.extra_fields:
        result["extra_context"] = {"env": "ci", "tags": ["portable", "local"]}

    sys.stdout.write(json.dumps(result) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
