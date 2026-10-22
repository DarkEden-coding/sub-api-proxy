# SAP real-client end-to-end harness

This harness runs installed Claude Code and Codex executables through two debug
`cliproxy` processes and `sap-router` against a loopback Python mock. All keys are
fake. No real model, provider endpoint, or login is used. Config API keys are now
logical SAP accounts, so assignment, affinity, exhaustion moves, and failover run
through the full chain.

## Run

Requirements: macOS with `sandbox-exec`, Python 3.9+, Perl, Rust, Claude Code,
and Codex CLI. The recorded run uses Claude Code 2.1.309 and Codex 0.163.0.
Run from the repository root, using a new output directory each time:

```sh
perl -e 'alarm 1200; exec @ARGV' env CARGO_BUILD_JOBS=4 nice -n 19 cargo build -p cliproxy -p sap-router
perl -e 'alarm 30; exec @ARGV' python3 -m unittest discover -s harness/sap-e2e -p 'test_*.py'
perl -e 'alarm 300; exec @ARGV' python3 harness/sap-e2e/run.py --output /tmp/sap-e2e-chain-new
perl -e 'alarm 300; exec @ARGV' python3 harness/sap-e2e/run.py --client-only --output /tmp/sap-e2e-clients-new
```

`results.json` records `pass`, `fail`, or `diagnostic`. Diagnostics have
`passed: null` and do not fail the run. Native ChatGPT bootstrap and intentional
client-only quota rejections are diagnostics. Unexpected client exits in required
chain scenarios fail. `run.json` records the source commit and mode;
`upstream.jsonl` contains sanitized request heads.

The default chain is:

```text
real client -> sap-router -> primary cliproxy --sap-config -> localhost mock
                         -> backup cliproxy --sap-config -> localhost mock
```

`--client-only` points the router directly at the mock to isolate client protocol
behavior. It proves no SAP account assignment or movement. The chain restarts the
primary with a fresh SAP database between Codex WebSocket and HTTP scenarios,
keeping its port and sandbox, so each scenario starts with two usable accounts.
Quota from one transport cannot make the next scenario skip exhaustion.

## Isolation and evidence

The runner creates private temporary `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
auth directories, fake keys, node configs, and SQLite files, then removes them
and stops its processes in `finally`. Every subprocess command has a Perl alarm.
CLI calls also have Python deadlines and closed stdin, except the two-turn Claude
case, whose pipe receives only synthetic JSON messages. Nodes and clients run
inside a macOS sandbox that denies non-loopback networking. Proxy environment
variables and cliproxy's explicit proxy point at the local deny proxy. CONNECT is
rejected. Real login directories are never used.

Only the four literal fake upstream keys become account labels. Authentication
headers, conversation content, system prompts, and tool arguments are omitted
from evidence. Full process logs stay in the output folder for local diagnosis;
review them before sharing. Checked-in evidence contains sanitized JSON and
selected synthetic client events.

## Mock controls

`mock.Mock(evidence: pathlib.Path, port: int = 0)` binds loopback:

- `exhausted` rejects selected fake accounts. Claude HTTP returns 429 with
  rejected unified-rate-limit headers and a limiting-window reset.
- Codex HTTP exhaustion returns 429 with `usage_limit_reached`, reset fields,
  `x-codex-limit-reached: true`, primary used percent, window, and reset headers.
  WebSocket exhaustion sends `codex.rate_limits` with `limit_reached: true`,
  then an error frame with status 429, quota headers, and the same error type,
  followed by close 1000. Prewarm remains successful.
- `exhaust_next_codex` exhausts the account selected for the next generating
  request. This avoids assuming a particular account wins policy scoring, and
  lets the WebSocket scenario establish a pin before injecting exhaustion.
- `force_continue_400` rejects the next Claude `thread.type=continue`, then
  accepts a full-history resend. This protocol fixture is distinct from SAP's
  own forced-recovery decision.
- `close_next` closes the next generating WebSocket request normally.
- `codex_tool` sends a harmless `exec_command` call, then closes the next request,
  testing recovery after an established response ID.
- `scenario` labels subsequent evidence records.

The first Claude request advertising `Agent` or `Task` receives a tool response
that launches a local child. Later requests receive text. Codex fixtures support
HTTP SSE, masked/fragmented WebSocket text frames, ping/pong, prewarm, completion
events, and normal close. These scripted fixtures do not emulate provider thread
storage or validate real subscription quotas. Three loopback protocol tests cover
Claude tool/SSE output, forced 400, both providers' exhaustion signals, Codex SSE,
and WebSocket error/normal-close frames.

## Recorded results, October 22, 2026

The final full-chain run exits 0. Evidence is in `evidence/chain/`; the separately
rerun client/router/mock diagnostics are in `evidence/clients/`.

| Scenario | Result |
| --- | --- |
| Claude tool round trip, child assignment/affinity, and resume | Pass |
| Claude two turns in one process, exhaust current account between turns | Pass, next request moves and completes |
| Claude native forced 400 and full-history resend | Not triggered in custom-base mode |
| Codex custom provider, WebSocket and HTTP SSE | Pass |
| Codex generating-turn exhaustion, WebSocket and HTTP | Pass, same turn completes on the other account |
| Native ChatGPT mode and ChatGPT exhaustion | Diagnostic, bootstrap endpoints are not emulated |
| Claude exhaustion and backup continuation | Pass |
| Real-client failover after killing primary | Pass, under five seconds |
| Direct mock Codex full-input resend after normal socket close | Pass, client-only diagnostic |

The two-turn Claude case uses one process with streaming JSON input. Its first
request succeeds, the mock exhausts that account between turns, and the next
request gets an upstream 429 before succeeding on the other account. No `thread`
field is sent. The requests already carry full history, so this move needs no
forced 400 or client resend. The runner records that limitation explicitly and
fails conservatively if native continuation appears without verified recovery.
Synthetic SAP integration tests separately cover forced 400 until full history
is resent. Restarting a client with `--resume` is not forced-resend evidence.

Codex's WebSocket scenario accepts prewarm, rejects the generating request on
that socket, and completes the same turn using the other account. Its client log
contains `turn.completed` and no `turn.failed` or usage-limit error. HTTP performs
the same move. SAP classifies the quota signals as `Outcome::Exhausted`; a plain
429 without those signals remains `RateLimited`.

The prior generic hook 400 had two causes: incomplete mock quota signals led SAP
to retry the rate-limited account, while the hook's sentinel error discarded its
actual status/body outside the initial HTTP reply path. WebSocket dispatch also
excluded every account except its old pin, preventing a confirmed-exhaustion move.
SAP now owns that selection. Late hook replies carry their status and native JSON
through Claude/Codex SSE writers, and WebSocket replies bypass upstream-only
suppression rules. Untracked requests and runs without hooks retain the existing
selection behavior.

Native ChatGPT mode still needs real bootstrap/discovery protocol fixtures.
It times out without inference here and is not counted as a failure. Actual remote
compaction and native Claude server-thread recovery remain unverified. No live
provider test, deployment, release, push, or whole-workspace build was performed.
