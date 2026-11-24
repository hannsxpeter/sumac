# Failed Jobs Fixture (`sxmc check`)

This portable fixture demonstrates a silent schema-drift failure ("false zero")
and how `sxmc check` catches it before CI or agents accept empty results as
healthy.

## What the fixture models

- `fake_jobs_cli.py`: simulates an upstream jobs CLI with two seeded jobs:
  - `job-10` (`nightly-index`, healthy)
  - `job-17` (`billing-export`, failed)
- `list_failed_jobs.py`: invokes `fake_jobs_cli.py` and filters jobs where
  `status == "failed"`.
- `.sxmc/checks.toml`: asserts `expect_exit = 0`, JSON types (including
  `"/last_error" = "null"` and escaped pointers `"/meta~1schema"` and
  `"/meta~0source"`), and exact values `"/failed_count" = 1` and
  `"/failed_job_ids" = ["job-17"]`.
- `checks-v2-drift.toml`: runs the same workflow against `--mode v2-drift`, where
  `fake_jobs_cli.py` renames `"status"` to `"state"`. `list_failed_jobs.py`
  still exits `0` with `failed_count = 0` and `failed_job_ids = []`, so an
  exit-code-only check would pass silently. `sxmc check` detects the mismatch
  and exits `1`.

## Run the fixture

From the repository root (after `cargo build`):

```bash
# Passing v1 baseline (human output, exit 0)
./target/debug/sxmc check --root "tests/fixtures/failed-jobs"

# Passing v1 baseline (machine-readable JSON, exit 0)
./target/debug/sxmc check --root "tests/fixtures/failed-jobs" --format json

# Schema-drift failure (false zero caught, exit 1)
./target/debug/sxmc check --root "tests/fixtures/failed-jobs" --manifest "tests/fixtures/failed-jobs/checks-v2-drift.toml"
```
