# Responses WebSocket fixtures

Both files were produced by CLIProxyAPI itself; the Rust tests never derive expected
values from Rust code. `ws_vectors.json` comes from `6fecc6e`. `ws_e2e.json` was
regenerated at v8.0.35 (`ae3ee3be`) for the shared `upstream.codex` steering scenarios
(Go 52d5507d); every other scenario came out unchanged there apart from random keys and
IDs, so those entries keep their `6fecc6e` bytes. Every credential is fake and the
upstream is a loopback mock, so nothing reaches OpenAI.

- `ws_vectors.json`: the handler's pure helpers (request normalization and incremental
  merge, passthrough, prewarm, SSE payload extraction, error payloads, request-fault
  exposure, completion output restoration, pending tool calls, tool-call repair against
  the shared caches, close-reason truncation). Replayed by
  `src/websocket_requests_tests.rs`.
- `ws_e2e.json`: forty-one scenarios through Go's real `ResponsesWebsocket` handler, auth
  manager, built-in translators and `CodexAutoExecutor`, against a scripted upstream that
  speaks both WebSocket and HTTP SSE. It records the frames and close codes the client
  saw, what upstream received (WebSocket dials and frames, HTTP requests) and the upgrade
  response's `x-codex-turn-state`. Replayed by `tests/ws_e2e.rs`, which masks UUIDs and
  the prewarm timestamp.

Scripted upstream closes wait 300 ms after their events. Without that pause Go's
disconnect notifier races the turn and can close the client before the events are
written; the fixtures record the intended order.

## Regenerating

```sh
git -C <CLIProxyAPI> worktree add /tmp/cpa-ws ae3ee3be
cp go/zz_rsfix_ws_test.go /tmp/cpa-ws/sdk/api/handlers/openai/
(cd /tmp/cpa-ws && RSFIX_OUT=/tmp/rsfix-ws go test -count=1 -run TestRSFixWS ./sdk/api/handlers/openai/)
cp /tmp/rsfix-ws/*.json .
```
