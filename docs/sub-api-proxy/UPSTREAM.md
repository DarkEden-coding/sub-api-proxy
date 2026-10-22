# Upstream base

Sub-Api-Proxy is a fork of [vayungodara/cliproxy-rs](https://github.com/vayungodara/cliproxy-rs).

| | |
| --- | --- |
| Base commit | `81afb744` (v8.0.35 parity, "Gemini to Claude responses and the Claude tool argument cleaner") |
| Remote | `upstream` → `https://github.com/vayungodara/cliproxy-rs.git`, branch `master` |
| Merge policy | Merge upstream tags or commits into `main`; never rebase `main`. Record each merge here. |

Sub-Api-Proxy code lives in `crates/sap-*`, `ui/sap/` and `docs/sub-api-proxy/`. Changes to upstream files are limited to the integration points in [implementation-plan.md](implementation-plan.md) and marked with `// sub-api-proxy:` comments.

## Merges

| Date | Upstream commit | Notes |
| --- | --- | --- |
| 2026-10-22 | `81afb744` | Initial base |
