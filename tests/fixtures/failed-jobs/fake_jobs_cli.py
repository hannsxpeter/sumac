#!/usr/bin/env python3
"""Fake jobs CLI fixture for workflow schema-drift checks."""

import argparse
import json
import sys


def main() -> int:
    parser = argparse.ArgumentParser(description="Fake jobs CLI fixture")
    parser.add_argument(
        "--mode",
        choices=["v1", "v1-extra", "v2-drift", "nonzero"],
        default="v1",
        help="Response mode to simulate",
    )
    args = parser.parse_args()

    if args.mode == "nonzero":
        sys.stderr.write(json.dumps({"error": "upstream unavailable"}) + "\n")
        return 3

    if args.mode == "v1":
        payload = {
            "api_version": "v1",
            "jobs": [
                {"id": "job-10", "name": "nightly-index", "status": "succeeded"},
                {"id": "job-17", "name": "billing-export", "status": "failed"},
            ],
        }
    elif args.mode == "v1-extra":
        payload = {
            "api_version": "v1",
            "region": "us-east-1",
            "cursor": None,
            "jobs": [
                {
                    "id": "job-10",
                    "name": "nightly-index",
                    "status": "succeeded",
                    "worker": "node-1",
                    "duration_ms": 120,
                },
                {
                    "id": "job-17",
                    "name": "billing-export",
                    "status": "failed",
                    "worker": "node-4",
                    "duration_ms": 420,
                },
            ],
        }
    else:
        # v2-drift: upstream renamed "status" to "state" while still returning job-17.
        payload = {
            "api_version": "v2",
            "jobs": [
                {"id": "job-10", "name": "nightly-index", "state": "succeeded"},
                {"id": "job-17", "name": "billing-export", "state": "failed"},
            ],
        }

    sys.stdout.write(json.dumps(payload) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
