# Client configuration

Decided October 22, 2026 from the end-to-end harness (`harness/sap-e2e`) and a mock check of Claude Code 2.1.309. No real model was called.

## Claude Code

| Configuration | Subscription betas (`oauth`, `extended-cache-ttl`) | Request class, agent type, compaction headers | Message threads, shared cache scope |
| --- | --- | --- | --- |
| Direct to `api.anthropic.com` (reference) | yes | yes | yes |
| `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` | no | only with hint headers | no |
| Same, plus `ENABLE_PROMPT_CACHING_1H=1` | no 1-hour beta was sent | only with hint headers | no |
| **`ANTHROPIC_BASE_URL` + subscription login + `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1`** | **yes** | **yes** | no |

The chosen setup is the last row. Claude Code's gateway documentation says that a base URL alone does not replace the subscription login, and that `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` turns on the request-class, agent-type, compaction and prompt-ID headers for custom endpoints. A credential variable switches Claude Code to key mode, which drops the subscription betas, including the 1-hour cache TTL.

How it works:

- `sap setup claude` sets the base URL and hint headers, and removes credential variables.
- The local router has `client_key_file`. On `/v1/` and `/backend-api/` paths it replaces the client's `Authorization` with the client key and drops `x-api-key`. The coding client's own OAuth token never leaves the machine. Management paths keep their own credentials.
- Message threads and the shared cache scope are reserved for direct connections. Without message threads, every Claude request carries the full history. Moving a thread between accounts or servers therefore needs no forced resend for Claude; only the prompt cache is lost, as the routing rules already allow.
- Compaction is labelled by `x-claude-code-compaction` again, so routing can re-score after compaction without hooks.

## Codex CLI

A custom provider keeps every native signal the proxy uses: WebSocket transport with prewarm, `thread-id`, `session-id`, `x-codex-turn-metadata` and the window ID. `chatgpt_base_url` needs ChatGPT's real bootstrap endpoints and was not usable against a local mock.

```toml
model_provider = "sub-api-proxy"

[model_providers.sub-api-proxy]
name = "Sub-Api-Proxy"
base_url = "http://127.0.0.1:8317/v1"
wire_api = "responses"
supports_websockets = true
env_key = "SAP_CLIENT_KEY"
```

After a normal WebSocket close, Codex reconnected through the router in 299 ms and resent its full history without `previous_response_id`. This is the path the proxy uses to move a Codex thread.
