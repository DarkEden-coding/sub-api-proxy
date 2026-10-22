# SAP real-client end-to-end harness

This harness uses installed Claude Code and Codex executables, two debug
`cliproxy` processes, `sap-router`, and a Python standard-library upstream mock.
It does not call a model or use real credentials. The current SAP chain fails
before inference because configured API keys are not discovered as subscription
accounts. The evidence records that failure; this is not a passing E2E suite.

## Run

Requirements: macOS with `sandbox-exec`, Python 3.9+, Perl, Rust, Claude Code
2.1.308, and Codex CLI 0.163.0. Run from the repository root:

```sh
CARGO_BUILD_JOBS=4 nice -n 19 cargo build -p cliproxy -p sap-router
python3 -m unittest discover -s harness/sap-e2e -p 'test_*.py'
python3 harness/sap-e2e/run.py --output /tmp/sap-e2e-chain-new
python3 harness/sap-e2e/run.py --client-only --output /tmp/sap-e2e-clients-new
```

Each output directory must be new. Both integration commands currently return
exit 1 because required contracts are unmet. `results.json` contains individual
assertions, `run.json` identifies the mode and source commit, and `upstream.jsonl`
contains sanitized request heads. Client exit failures remain failures even when
an injected error is the expected diagnostic outcome. `--client-only` configures
the router to send clients to the mock, bypassing cliproxy to distinguish client protocol behavior from SAP
integration failures. It does not prove routing or account affinity.

The default chain is:

```text
real client -> sap-router -> primary cliproxy --sap-config -> localhost mock
                         -> backup cliproxy --sap-config -> localhost mock
```

The runner writes private temporary `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
auth directories, client keys, node configs, and SQLite files. It removes them
and stops its processes in `finally`. Every CLI invocation, including version
checks, has closed stdin and `perl -e 'alarm shift; exec @ARGV' N ...`, plus a
Python subprocess timeout. Children run inside a macOS sandbox that denies
non-loopback networking. All proxy environment variables and cliproxy's explicit
proxy point at a local deny proxy. CONNECT is rejected. No real login directory
is read or written by the harness. The installed executable may itself reside
under the user's home directory.

Only the four known fake upstream keys become account labels in evidence.
Authentication headers, full user messages, system prompts, and tool arguments
are not recorded. Full client/process logs are kept in the selected output
folder for diagnosis; review them before sharing. Checked-in evidence contains
only synthetic identifiers and selected excerpts.

## Mock controls

`mock.Mock(evidence: pathlib.Path, port: int = 0)` binds loopback. Tests can set:

- `exhausted`: fake credential labels to reject, such as `fake-claude-a` or
  `fake-codex-b`. HTTP returns 429. Claude gets rejected unified-rate-limit
  headers and a reset time; Codex WebSocket gets `limit_reached`, then close 1000.
- `force_continue_400`: reject the next Claude `thread.type=continue` with 400
  `invalid_request_error`, then accept a full-history resend.
- `close_next`: close the next generating WebSocket request normally.
- `codex_tool`: emit a harmless `exec_command` tool call, then close the next
  request. This tests recovery after an established response ID.
- `scenario`: label subsequent evidence records.

The first Claude request advertising `Agent` or `Task` receives a `tool_use`
response that launches a local child. Subsequent turns receive text. Codex
supports both HTTP SSE and masked/fragmented WebSocket text frames, ping/pong,
normal close, prewarm, and Responses completion events. These are small scripted
protocol fixtures, not provider emulators. They do not validate real provider
thread storage or subscription quotas.

A standalone mock can be started with a JSON script:

```sh
printf '%s\n' '{"exhausted":["fake-claude-a"],"close_next":true}' > /tmp/sap-mock-script.json
python3 harness/sap-e2e/mock.py --port 9009 --script /tmp/sap-mock-script.json --evidence /tmp/sap-mock.jsonl
```

The two protocol tests check Claude SSE/tool output, quota headers, forced 400,
OpenAI SSE, and a masked WebSocket close. They use only loopback sockets.

## Credential blocker

`cpa_core::config::credentials` builds config API-key credentials with `api_key`
and `base_url` attributes, but without arbitrary subscription metadata. Neither
`account_uuid` nor `account_id` is an accepted API-key config setting.
`sap_server::accounts::identity` requires `account_uuid` or `email` for Claude,
and `account_id` for Codex. Therefore the four local API-key credentials yield
zero SAP accounts. `GET /sap/api/accounts` returns `[]`; threads are identified
but have `account: null`. Claude receives 429 and retries until its alarm. Codex
fails with a routing-hook 400 before reaching the upstream mock.

An auth JSON file retains subscription metadata, but file synthesis does not
promote Claude/Codex `base_url` or `api_key` into executor attributes. Both
executors read their base URL from attributes. Merely adding a localhost
`base_url` to a fake OAuth JSON file would leave the default provider URL active.
This harness deliberately does not run that unsafe setup. File credentials also
receive OAuth auth-kind classification. The closest safe setup here is two
configured API keys per provider with distinct fake key labels and local base
URLs. It cannot prove subscription routing.

Integration needs a supported way to attach identity metadata to local API-key
credentials, or a supported local-base override for file credentials. This is an
adapter/configuration request, not a `sap-core` contract change. Production
crates were not changed to make tests pass.

## Recorded results

The evidence under `evidence/chain/` is the SAP chain; `evidence/clients/` is a
separate client/router/mock diagnostic. Runs were on October 22, 2026. The debug
build and both protocol tests passed. No workspace-wide tests were built.

- Claude custom base URL and auth token preserve session IDs, child agent IDs,
  tools, and resume. The tool round trip has message counts 1, 1 for the child,
  then 3 and 5 for the parent. Resume sends seven messages.
- Claude custom-base mode does not send `thread`, the message-thread beta,
  request-class, or agent-type headers in this run. Native server-thread recovery
  cannot be exercised in this configuration. A 429 is retried until the deadline.
- Codex custom `model_providers` with `wire_api = "responses"` and
  `supports_websockets = true` preserves WebSocket prewarm, `thread-id`,
  `session-id`, `x-codex-turn-metadata`, the window ID, and
  `remote_compaction_v2` advertisement. Actual remote compaction was not proven.
  Setting `supports_websockets = false` succeeds using HTTP SSE.
- Codex `chatgpt_base_url` with synthetic ChatGPT tokens stops at routing
  discovery with `selected workspace missing from routing discovery`. It does
  not reach inference. Choosing the custom provider is supported by this run;
  native ChatGPT-mode compatibility remains inconclusive without a richer local
  bootstrap mock. No real token or real discovery endpoint was used.
- Diagnostic Codex quota injection produces `turn.failed` and the usage-limit
  message. This is distinct from a normal socket-close retry.
- SAP assignment, parent affinity, account exhaustion movement, Claude forced
  400/full resend, and real-client failover remain failing or blocked by account
  discovery. A newly launched `--resume` process is full history by construction;
  it is not evidence of forced in-process resend.

After a tool response established `resp_local_9`, the close test received a
one-item continuation on connection 7. Codex reconnected on connection 10 after
299 ms, sent six input items, and omitted `previous_response_id`. The turn then
completed. This proves client recovery from a normal upstream close through the router
against the mock; SAP's `CloseSocket` path remains unverified.

A `/compact` diagnostic completed, but its mock request still had no compaction
or request-class header. The local command's success alone does not establish
native compaction routing.

The committed JSON and client excerpts give exact timing and reconnect details.
A synthetic untracked HTTP request tests transport failover independently of
account discovery. It reached the backup and returned 200 in 25 ms after primary
termination, using default router probe settings. Its success must not be
reported as real-client failover.
