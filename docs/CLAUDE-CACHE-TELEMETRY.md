# Claude cache telemetry

Claude Messages attempts add `claude_cache` to the existing usage queue and RESP
`usage` channel when usage statistics are enabled. This is observation only: it
does not change request bytes, cache markers, TTLs, routing, or responses. It never
sends maintenance requests or token-count requests. Usage plugins retain the Go
record format.

## Record fields

`claude_cache.schema_version` is `1`. Each record covers one upstream attempt,
including failed attempts that reached request preparation.
Tokenless streams still produce a queue observation, without a Go plugin record.

- `breakpoints` lists the final upstream cache markers in tools, system, and
  messages order. Each entry has `path`, `ttl`, a cumulative `prefix`, and
  `lookback` fingerprints within Anthropic's 20 lookup positions, including the
  breakpoint itself. Consecutive tool-use blocks and consecutive tool-result
  blocks share a position. An omitted TTL is recorded as Anthropic's default
  `5m`. Top-level automatic caching appears as `cache_control:<last-eligible-path>`:
  thinking and empty text suffixes are skipped. A same-TTL explicit marker at
  that target is a no-op, not a duplicate breakpoint. `automatic_cache_ttl`
  retains the top-level TTL even when no target exists or explicit TTLs conflict.
- `tools`, `system`, and `history_prefix` contain `hash`, `bytes`, and
  `tokens_estimate`. History ends at the last message cache breakpoint. No cached
  history leaves an empty fingerprint. Cumulative breakpoint fingerprints include
  the preceding tools, system, and message roles.
- `model`, `thinking_type`, `thinking_budget`, `thinking_hash`, `effort`, `betas`,
  `speed`, and `service_tier` describe the final body and beta headers. The full
  thinking configuration is hashed, not stored. Prefix hashes also bind the
  model, thinking, output configuration, tool choice, context management, speed,
  tier, and betas.
- `session_hash` hashes the final wire session, falling back to the dispatch
  session. `account_hash` hashes the selected credential's account UUID, falling
  back to its credential ID. The latter is stable only while that credential
  ID remains unchanged. OAuth token refresh does not change an account UUID.
  `scope_hash` also binds the wire account UUID, upstream base URL, caller isolation, and final
  organization, project, and workspace headers. No identifiers or header values
  are stored in these additions. Without an explicit client session, a derived
  session can change when its instructions change.
- `request_start_unix_ms`, `first_response_byte_unix_ms`, and `end_unix_ms` are UTC
  Unix milliseconds. Start is the upstream send, not client arrival. End is the
  attempt's completion or cancellation, not the downstream socket flush. Missing
  stages are `null`, not a synthetic zero timestamp.
- `request_type` is `main`, `helper`, `subagent`, `compaction`, or `retry`.
  `base_request_type` retains the original kind for retries. `attempt` starts at
  zero and increments across proxy failover and credential-refresh retries within
  one client request. Existing `request_id` joins those attempts. A positive
  client `x-stainless-retry-count` also classifies a request as `retry`.

Known Claude helper and subagent signals and the server's compaction metadata
determine the kind. A producer can supply the logging-only
`x-cliproxy-request-type` header with one of those five values when its purpose is
not visible to the proxy. The header does not select a traffic policy. Without
such a signal, an ordinary Messages request is `main`; the proxy cannot reliably
infer an Amp compaction or scheduled wake-up from prompt text.

The existing `tokens` object adds `cache_creation_5m_tokens` and
`cache_creation_1h_tokens` from Anthropic's
`cache_creation.ephemeral_5m_input_tokens` and
`cache_creation.ephemeral_1h_input_tokens`. SSE output-only updates retain earlier
cache buckets. A returned zero is stored. An omitted bucket stays absent: it is
not inferred from the requested TTL. Aggregate reads and writes remain unchanged.
Returned TTL buckets survive an executor-published failure even when Go's
aggregate failure accounting reports zero tokens.

## Fingerprint and token-count limits

Hashes are full SHA-256. Identity hashes have separate domain prefixes. Content
hashes use the final prepared JSON blocks, paths, and roles, with length framing.
Only the blocks' `cache_control` objects are omitted, so moving a marker or choosing
another TTL does not change the matching content fingerprint. JSON string escapes
are normalized by decoding and serializing the observation copy. Field order,
text, tool names, signatures, and other content remain significant. These are
proxy fingerprints, not Anthropic's private cache keys.

`token_estimator` is `o200k_base_json_blocks_256b_chunks`. It uses the existing
local O200k tokenizer in bounded, UTF-8-safe chunks. This bounds regex and BPE work
on long repeated strings. Counts include JSON block syntax, omit provider framing,
and do not implement Anthropic's media tokenization. Chunk seams can change the
estimate. **These are not exact Anthropic section counts.** Anthropic returns exact
aggregate usage, not per-section lengths. Exact section counts would require
additional provider calls, which this feature does not make.

Work is linear in body size, with a bounded lookback ring. Tokenization and the
observation copy are skipped when the usage queue is disabled. Enabling telemetry
also initializes the existing shared O200k tokenizer if it was not already used.

The new fields contain no prompt content or credentials. Existing Go-compatible
usage fields are unchanged and can contain client keys and account metadata. A
collector must protect the full record or export an allowlist containing
`execution_id`, `request_id`, `failed`, `tokens`, and `claude_cache`.

## Offline replay

`bench/claude-cache-replay.py` accepts JSONL, one usage record per line, and reports
the last seven days ending at the latest attempt end. Earlier supplied records
warm the simulation without contributing to that week's costs. Duplicate
`execution_id` values are skipped. A short export reports its coverage rather
than claiming to contain a week.

```sh
python3 bench/claude-cache-replay.py week.jsonl --wakeups wakeups.jsonl > comparison.json
python3 bench/test-claude-cache-replay.py
```

The existing usage queue retains records for 60 seconds by default, at most one
hour. It is not a week-long archive. A collector must continuously drain the queue
or consume the RESP channel into a protected JSONL file. This change adds no
collector or live service. Missing collection intervals cannot be reconstructed.

The report compares these policies:

- `observed`: replay the TTLs actually sent, to check the model against real usage.
- `5m`: every existing breakpoint uses five minutes.
- `1h`: every existing breakpoint uses one hour.
- `hybrid`: tools and system breakpoints use one hour; message breakpoints use five
  minutes. Breakpoint positions remain unchanged.
- `1h+bounded-pings`: simulate a matching-prefix refresh after 57 minutes without
  matching activity, at most three pings and four hours after real activity.
  Pings are permitted only after successful `main` attempts. Real session activity
  cancels the old timer. Errors, compaction, helper requests, and scope or prefix
  changes prevent that old timer from firing. An expired prefix fails the local
  preflight and stops the chain.

The simulation anchors sliding expiry at request start and makes a cold entry
available at the first response byte. Concurrent cold starts can both write. It
uses the longest matching live prefix within the recorded lookback, accounts for
new tail writes, and separates account and workspace scopes. Prefix positions are
calibrated to each attempt's exact returned cache-read plus cache-write total.
The relative positions of mixed-TTL segments remain estimates.
`replay_validation` compares the observed-TTL simulation with actual reads and
writes. Large residuals indicate missing cache state, collection gaps, or model
error; do not treat policy cost differences as measured savings in that case.

Default prices are Opus 5.5 API-equivalent dollars per million tokens: $4 input,
$5 five-minute writes, $8 one-hour writes, $0.20 cache reads, and $20 output.
Other models and fast requests are excluded from priced totals, but retained as
activity that cancels session timers and can flag wake-up writes. `--model`, `--input-usd`,
`--output-usd`, and `--read-multiplier` select another pricing scenario. Pings add
128 assumed uncached input tokens and zero output by default. Adjust
`--ping-input-tokens` to test overhead. This is an optimistic simulation of
successful refreshes, not evidence that OAuth prewarming works or is permitted.
There is no subscription-quota conversion.

Real input and output charges, including observed compaction and retries, stay in
each policy. Requests without usable cache measurements are not invented: their
observed usage remains charged, and `unmodelled_requests` identifies the gap.
Unknown write TTLs contribute a cost range, `api_equivalent_usd_low` through
`api_equivalent_usd_high`. Known and unknown writes are computed per attempt before
summing, so zeroed failure aggregates cannot erase another attempt's unknown-TTL
writes. `observed_tokens.cache_creation_tokens` retains the Go aggregate;
`effective_cache_creation_tokens` also includes exact buckets preserved on
failures. Replay validation compares against that effective total.
Left-edge cold state, unobserved eviction or routing,
incomplete exports, and the unknown return after the right edge limit conclusions.
Pings before the right edge cost money even if the user never returns. Future
compaction behavior, additional maintenance output, quota pressure, logout, and
unlogged errors cannot be inferred from these records.

## Scheduled wake-up flags

An optional wake-up export supplies one JSON object per line:

```json
{"timestamp":"2026-10-15T08:00:00Z","session_hash":"<matching claude_cache.session_hash>"}
```

The timestamp can also be Unix milliseconds. Use the hash observed for that
scheduled thread, not its raw thread ID or credentials. The report flags cold
writes within two minutes after each wake-up. It distinguishes identical-prefix
rewrites, overlap before the earlier first byte, and changed scope, model,
thinking, effort, beta, tools, system, or history fingerprints.

These are candidate events, not a double-write diagnosis. A schedule export is
required because the proxy cannot observe the Amp scheduler. No export produces
`wakeups_provided: false`, not an assertion that scheduled double writes did not
happen.

## Deployment boundary

Mock-upstream tests verify both response modes, privacy of the additions, final
TTLs, and byte-identical traffic with telemetry on and off. CI also runs the
network-free replay fixtures. A week of new production telemetry is still needed
before choosing a TTL or evaluating a maintenance policy. Publishing a release
or replacing Core's running proxy requires Vayun's explicit approval. No live
Claude login is exercised by these tests.
