# Pre-implementation validation

Recorded October 22, 2026, with Claude Code 2.1.308 and Codex CLI 0.163.0 on macOS. This covers validation items 1 and 2 from [discovery.md](discovery.md).

## Method

Both clients ran unmodified through a local mitmproxy instance (`research/capture/`). A mock addon answered every inference request with scripted replies, so no model was called after one initial smoke request per client. Other endpoints, such as bootstrap and plugin lists, reached the real services. Hooks for both clients logged their stdin payloads. Scenarios covered a new thread with a tool call and a subagent, resume, fork, manual and automatic compaction, and three fault cases.

Raw captures are private and excluded from version control.

## Item 2: thread, child, fork, and compaction identity

Request metadata identifies every case. Hooks are not required for routing. Their identifiers match the requests and can serve as a cross-check.

### Claude Code

| Case | Signal on the request |
| --- | --- |
| Thread | `X-Claude-Code-Session-Id` header, repeated in `metadata.user_id.session_id`. It stays the same across resume. |
| Request type | `x-claude-code-request-class`: `main`, `auxiliary` (titles and similar), `subagent`, `compaction`. |
| Subagent | Parent session ID, plus `x-claude-code-agent-id` and `x-claude-code-agent-type`. The billing system block adds `cc_is_subagent=true`. |
| Fork | New session ID. `diagnostics.previous_message_id` is the parent thread's last message ID, which links the fork to its parent. The `SessionStart` hook reports `source: fork`. |
| Compaction | `request-class: compaction` and `x-claude-code-compaction: manual` (or `auto`). Compaction runs as an internal agent. Afterwards, `SessionStart` reports `source: compact`. |
| Turn | `x-claude-code-prompt-id` and `cc_prompt_id` / `cc_turn_index` in the billing block. |

**Server-side threads.** Main requests use the `message-threads-2026-08-12` beta. The first request sends `thread: {type: create}` with the full history. Later requests in the same process send `thread: {type: continue, previous_message_id}` with only the new messages and no tool list. Resume, fork, and the first turn after compaction send `create` with the full history.

**Fallback behavior.** When the mock answered a `continue` with HTTP 400 `invalid_request_error`, Claude Code immediately resent the full history as `create`, and the turn succeeded. A 404 `not_found_error` produced a retry of the same `continue`.

**Caching.** System blocks carry `cache_control` with `ttl: 1h`, and one block has `scope: global`. The first system block is a billing header with a checksum (`cch=`). The proxy must forward these blocks unchanged.

### Codex CLI

Codex sends inference over a WebSocket to `chatgpt.com/backend-api/codex/responses` (`openai-beta: responses_websockets=2026-02-06`, `x-codex-beta-features: remote_compaction_v2`). Each agent thread opens its own connection, and each connection starts with a prewarm request (`generate: false`).

| Case | Signal on the request |
| --- | --- |
| Thread | `thread-id` header and `client_metadata.thread_id`. `session-id` is the root session. |
| Subagent | Separate connection. Its `thread-id` is the child's ID, `x-codex-turn-metadata.parent_thread_id` names the parent, `thread_source: subagent`, and `agent_name` is a path such as `/root/reader`. `prompt_cache_key` is the root session ID, shared by parent and child. |
| Fork | New `thread-id`, `forked_from_thread_id`, and `history_initialization: cold_fork`. |
| Resume | Same `thread-id` and `history_initialization: cold_resume`. |
| Compaction | `request_kind: compaction`, an input that ends with a `compaction_trigger` item, and `compaction: {trigger, reason, implementation, phase, strategy}` in the turn metadata. The response must contain one `compaction` item. |
| Compaction generation | `x-codex-window-id` is `<thread>:<n>`, and `n` increments after each compaction. |

**Connection-scoped state.** Turns chain with `previous_response_id` on one connection, and later requests carry only new items. A new connection, a resume, and the first request after compaction send `previous_response_id: null` with the full input.

**Quota.** Each response stream includes `codex.rate_limits` with `plan_type`, primary and secondary windows (`used_percent`, `window_minutes`, `reset_at`), and credits. This supports automatic account discovery and weighting.

**Silent failure.** When the mock swallowed a request and never answered, Codex waited more than 90 seconds without reconnecting or failing. The proxy needs its own idle and first-byte timeouts to meet the five-second failover target. How Codex reacts to an explicit close was not confirmed with mitmproxy. Test it in the proxy's own mock-upstream harness.

### Design implications

1. Route by thread key: Claude `session_id` + `agent_id`, and Codex `thread-id`. Parent links come from `agent_id` (Claude), `parent_thread_id` / `forked_from_thread_id` (Codex), and `previous_message_id` (Claude forks).
2. A thread can change account or server only at a point where the client sends full history, or where the proxy forces one. For Claude, a 400 on `continue` triggers a full resend. For Codex, closing the client connection should trigger one, but this still needs confirming.
3. These fallbacks mean the proxy does not need to store full transcripts to recover. A move costs one full-context request and loses the provider cache, as the agreed routing rules already allow.
4. Compaction detection needs no hooks or size heuristics. Both clients label it on the request.

## Item 1: independent login sessions

Skipped by decision on October 22. The [implementation plan](implementation-plan.md) supports either outcome and repeats the check in phase 0. It needs two browser logins to the same subscription for each provider. The harness is in `research/session-test/`:

1. `./login.sh claude a`, then `./login.sh claude b` (same Claude account both times). Repeat with `codex`.
2. `./run.sh claude` and `./run.sh codex`.

The test alternates refreshes of session A and session B, runs one concurrent refresh, and after each step checks both sessions against profile and usage endpoints. It makes no inference calls. Tokens are written back after each refresh, so both logins remain usable.
