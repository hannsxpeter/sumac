<!-- sxmc:begin cli-ai:portable -->
## Sumac CLI Surface: `sxmc`

Use `sxmc` (Sumac) as a portable terminal workflow across AI tools in this repo.

Summary: Sumac — bring out what your tools can do (Skills × MCP × CLI)

Recommended startup guidance:
- When the exact CLI surface is unclear, start with `sxmc inspect cli <tool> --depth 1 --format json-pretty`.
- For this CLI specifically, `sxmc` `--help` is still a good follow-up once you know you are on the right command.
- When the MCP surface is unknown, start with `sxmc stdio "<cmd>" --list` or `sxmc mcp grep <pattern>`.
- When the API surface is unknown, start with `sxmc api <url-or-spec> --list`.
- Prefer machine-friendly flags like `--json` when available.
- Keep bulky output in files or pipes instead of pasting it into chat context.
- Re-check auth or environment requirements before write actions.
- Host profile conventions in this repo were last verified on 2026-03-21.

High-confidence subcommands:
- `serve`: Start the MCP server (serves skills over MCP)
- `skills`: Manage skills
- `stdio`: Connect to an MCP server via stdio (MCP Server → CLI)
- `http`: Connect to an MCP server via HTTP (MCP Server → CLI)
- `mcp`: Use baked MCP connections in a token-efficient, mcp-cli-style workflow
<!-- sxmc:end cli-ai:portable -->

## Project Rules

- Language/Edition: Rust 2021 (`Cargo.toml`, `Cargo.lock`).
- Production paths return `crate::error::Result<T>` (`SxmcError`) with contextual error messages; avoid `.unwrap()` outside tests.
- Keep machine-readable structured output on `stdout` and diagnostics/warnings on `stderr`.
- Shell scripts under `scripts/` use `set -euo pipefail`.

## Checks

Derived from `.github/workflows/ci.yml`:

- **Fast:**
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --locked --test cli_integration`
- **Full:**
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo build --locked`
  - `cargo test --locked --all-targets`
  - `bash scripts/startup_smoke.sh target/debug/sxmc`
  - `bash scripts/smoke_portable_core.sh target/debug/sxmc .`
  - `bash scripts/smoke_portable_fixtures.sh target/debug/sxmc tests/fixtures`
  - `SXMC=target/debug/sxmc bash scripts/test-sxmc.sh --json /tmp/sxmc-test-results.json`
  - `bash scripts/smoke_test_clients.sh target/debug/sxmc tests/fixtures`
  - `node --check packaging/npm/bin/sxmc.js && SXMC_NPM_SKIP_DOWNLOAD=1 node packaging/npm/scripts/install.mjs`
  - `cargo package --locked`
- **Opt-in host smoke (requires `codex` and `python3`):**
  - `bash scripts/smoke_codex_app_server.sh target/debug/sxmc`
