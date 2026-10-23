# Workstream H implementation report

Implemented on `ws/sap-node`. No push or deployment. `control.rs` and `sap-core`
are unchanged. Workstream I's `api/` files are untouched.

## Delivered

- YAML configuration, automatic subscription-account discovery, local credential
  mappings, plan capacity, quota parsing, EWMA learning, and manual overrides.
- `Node: Control + SapHooks`: registry observation, parent affinity, policy
  decisions, outcome classification, cumulative-attempt usage deltas, response
  IDs, stream accounting, successful compaction, and Codex window advancement.
- Forced full-history recovery for changed accounts, changed serving nodes,
  missing records, manual moves, and unknown previous response IDs. A move clears
  the retained response ID, so recovery remains required after reopening SQLite.
- SQLite thread persistence, restored assignments, bounded failure buckets,
  account-state persistence, events, retention, authenticated sync routes, peer
  agents, remote assignment import and tombstone reconciliation. New configured
  peers start with an empty ACK cursor to retain their unacknowledged history.
  Exhaustion from newer peer quota observations applies locally; login revocation
  remains local to the credential-owning node.
- Account/thread/node views, selector weights, flow statistics, reset-aware
  responses, broadcast notifications, account patches, manual assignment,
  conflict resolution, and OAuth through the existing management router.
- `cliproxy --sap-config <path>` installs hooks before router construction. Its
  management instance is attached to the node for OAuth, using a private random
  loopback password when the existing local password is empty. It uses the same
  OAuth session store and callback sink as ordinary management.

Request bytes are only inspected by `sap-threads`; this crate does not rewrite
request bodies. No credential values or request bytes are logged.

## Public API

```rust
pub fn install(rt: &Arc<cpa_server::Runtime>, config: &Path)
    -> anyhow::Result<Arc<Node>>;

impl Node {
    pub fn attach_management(
        &self,
        management: &Arc<cpa_server::management::Management>,
        password: String,
    );
}

pub struct Node; // implements control::Control and cpa_server::sap_hooks::SapHooks
pub mod control; // existing trait and error/event types, unchanged
pub mod config;
impl config::Config {
    pub fn load(path: &Path) -> anyhow::Result<Self>;
}
```

Call `install` inside a Tokio runtime. Runtime holds the node's hooks; the node
keeps only a weak runtime reference. Background tasks stop when the node is
released. An embedding application that needs login must attach its existing
management instance and loopback password. Normal cliproxy startup does this.

## Configuration

```yaml
node: power-server
role: primary                  # primary or fallback
store_path: sap.sqlite          # relative to this YAML file
idle_window_secs: 20
registry_max: 10000
allow_loopback_peers: false     # explicit development/test opt-in
limits:
  quota_max_age_secs: 1209600
  usage_max_age_secs: 7776000
  event_max_age_secs: 604800
  op_log_max_age_secs: 1209600
  thread_max_age_secs: 2592000
  max_bytes: 268435456
peers:
  - id: macbook
    url: http://100.64.0.2:8317
    secret_file: peer.secret    # relative path; exact bytes, including any newline
routing:
  retry_deadline_secs: 60
  compaction_move_margin: 0.25
```

Other `routing` fields use `sap_core::routing::RoutingConfig` defaults and its
snake_case format. Limits, peers and routing may be omitted. Peer URLs must meet
`sap-sync`'s literal-IP Tailscale restrictions. Both sides need the same exact
secret-file bytes. The store's parent directory must already exist. Invalid,
duplicate, and self peer identities are rejected. Unknown YAML fields fail.

Claude account IDs are `claude:<account_uuid>` with email as fallback. Codex IDs
are `codex:<account_id>`. Config API keys without subscription metadata use
`<provider>:api-key:<credential_id>` as their logical account. Other credentials
without subscription identity fields are omitted. Account preferences survive rescans;
credentials stay local. A peer-known account without a usable local credential
appears as `login_needed_on` this node.

## Decision table to hook answers

- New root: `NewThread`, then best-score `Use` becomes `Credential`.
- New child/fork: `ChildStart` includes the parent's account. Parent affinity, or
  the fallback best account, becomes `Credential`.
- Ordinary continuation: `Continue` retains its account. A hard-ineligible
  account moves according to the policy. Model changes use model eligibility.
- 429 without confirmed exhaustion: `RateLimited`. 503/529: `Overloaded`.
  Both preserve the first-failure timestamp. `RetrySame` selects the current
  credential when its retry time has arrived. A future retry time returns a
  provider-shaped 429 with `retry_at`, because the hook is synchronous. At the
  deadline, `Reject` returns 429 and leaves the assignment intact.
- Claude rejected unified claims/windows, or Codex `limit_reached`, produce
  `Exhausted` with the observed limiting window and reset. Its `Use` excludes the
  failed account. Unknown reset times remain unknown.
- Explicit upstream model-support or model-access failures produce typed
  `ModelIneligible`, moving the affected thread without revoking the account.
  Other 401/403 failures produce local `Revoked`, then `Use` moves to another
  account. Generic 403 and quota exhaustion are not model-entitlement evidence.
- Successful compaction is counted once per request. The next selection uses
  `CompactionCompleted`; Codex window advancement can also infer this event.
  `Use` records the policy's kept/moved reason and account.
- No eligible account: `Reject` becomes provider-shaped 429, including the known
  earliest usable time in `retry_at`, or null when unknown.
- Any `Use` that invalidates a stateful continuation records the new assignment
  but forces recovery: Claude gets 400 `invalid_request_error`; Codex HTTP gets
  400 `previous_response_not_found` with `param: previous_response_id`; Codex
  WebSocket gets `CloseSocket`. Full-history requests can use the assignment.
- An unknown auxiliary request is left to configured upstream behavior without
  creating a registry record. A full registry rejects a new tracked request with
  429 rather than silently bypassing affinity. Buffered requests are protected
  from eviction until their request context is released.

## Validation

All commands use `CARGO_BUILD_JOBS=4 nice -n 19`. Build/test invocations have a
300-second subprocess timeout. No whole-workspace build or test was run.

- `cargo fmt -p sap-server -p cpa-server -p cliproxy`: passed.
- `cargo clippy -p sap-server --all-targets -- -D warnings`: passed.
- `cargo test -p sap-server`: eight integration tests passed.
- `cargo test -p cpa-server --lib sap_hooks`: four tests passed, including the
  added node-route merge assertion in the health test.
- `cargo test -p cliproxy --bin cliproxy every_go_flag_parses_in_single_and_double_dash_form`:
  passed, including the new `-sap-config` assertion.

Tests use private temporary auth directories and `cpa_server::testing::runtime`.
They cover affinity, child inheritance, exhaustion and repeated forced resend,
Codex hook close/HTTP errors, restored assignments, cumulative usage, manual
moves, temporary errors and deadlines, compaction deduplication, bounded
in-flight admission, HTTP pinning/observations/header stripping against a mock,
and two actual loopback nodes replicating an assignment. Existing G tests cover
actual WebSocket frame callbacks and normal socket close with a fake hook.
No real provider endpoint or LLM was contacted.

## Contract changes and remaining gaps

- Added only the default `SapHooks::routes() -> Option<axum::Router>` method to
  the public hook contract. `cpa_server::router` merges its already state-bound
  routes after binding runtime state. No `control.rs` changes.
- `reset_aware_refresh` actively reads Claude `/api/oauth/usage` and Codex
  `/backend-api/wham/usage` through executor-owned transports. Concurrent callers
  join one refresh, which continues if a caller disconnects. At most four local,
  enabled accounts are probed concurrently, with a ten-second deadline and 1 MiB
  response limit per account. No inference or token refresh is sent. API keys are
  reported as unsupported; peer-only and disabled accounts are skipped.
  Executor-owned first-party origins and explicit loopback mock origins are
  accepted. Credential base URLs and declarative probes are ignored, redirects
  are rejected, and tokens cannot cross provider boundaries.
- Successful active observations are stored and replicated with source `probe`;
  passive observations keep source `passive`. Fresh allowed usage clears stale
  exhaustion for threads still assigned to that account, without clearing
  revocation, model failures or short rate limits. Partial Claude windows cannot
  clear exhaustion for an unseen window. Polling reflects an active refresh;
  last-poll timestamps record attempts and errors contain only fixed messages
  and HTTP status codes. Failed probes retain prior observations.
  Poll attempt/error metadata is process-local and resets on restart; quota
  history and account exhaustion persist. Refresh is on demand, with no periodic
  usage traffic. Expired OAuth tokens require the normal credential refresh path.
- `SapPick` cannot express delayed selection or `Retry-After` headers. Future
  retry times therefore use 429 plus body `retry_at`, rather than waiting inside
  synchronous `select`.
- G's `CloseSocket` contract completes the current response using configured
  selection before closing. It does not close before dispatch or pin that final
  attempt to the newly assigned account. This behavior is inherited, not changed
  here. Real Codex reconnect/full-history recovery remains unverified.
- `AttemptError::ModelIneligible` carries explicit model-support/access evidence
  from upstream error bodies without exposing their text to SAP. The existing Go
  model-support classifier applies on 400/404/422, with explicit model evidence
  additionally accepted on 403. Quota 429s and generic 403s do not produce this
  signal. This moves the affected thread; it does not discover or persist a
  complete per-account model-entitlement catalog. Model changes also use
  `Account::serves_model` through the routing policy.
- The hook has no attempt ID. Usage deduplication is per request/credential;
  multiple cumulative counters restarted for the same credential within one
  request cannot be distinguished reliably. An attempt ID is needed for that case.
- Peer telemetry is available through authenticated sync, as described below.
  Health upstream flags still use successful traffic from the last five minutes,
  without synthetic probes.
- Learner parameters are fixed starting values pending replay calibration.
  Additional Codex quota families are not parsed beyond primary/secondary.
- OAuth URL generation reuses management, but browser completion was not tested.
  Existing management availability/auth requirements still apply. A re-login
  validates the requested account's provider but cannot force the browser user
  to choose that same subscription.
- Workstream I must mount its management/dashboard router alongside the sync
  routes. This branch intentionally does not implement or edit `api/`.

## E2E follow-up, October 22, 2026

The [real-client mock chain](../../harness/sap-e2e/README.md) now passes account
assignment, child affinity, confirmed Codex/Claude exhaustion moves, and backup
failover. SAP selection can replace a Codex WebSocket credential pin after
confirmed exhaustion before output. Late hook replies preserve native error
status/type in SSE and WebSocket frames. No public API or sap-core change was
needed. The custom-base Claude client sends full history without `thread`, so
its in-process move succeeds without triggering forced 400 recovery. Native
ChatGPT bootstrap and native Claude server-thread recovery remain unverified.

## Peer telemetry, October 22, 2026

`GET /sap/sync/telemetry` uses the sync endpoint's connection-address checks,
configured peer identity, timestamp window and HMAC over method, path and body.
The public, unauthenticated `/sap/health` response is unchanged. The telemetry
response contains only node ID, configured role, serving state, build version,
`observed_at`, request rate, open streams, selection latency and process RSS.
It contains no accounts, credentials, headers, thread IDs or request bodies.

Each configured peer is sampled every five seconds in a separate task. Requests
have a three-second deadline and two-second connect timeout. Both the server's
serialized response and the client's collected body are capped at 4 KiB. The
telemetry request body is also capped at 4 KiB. Unsupported endpoints, failed
samples and malformed responses never change replication's cursor, success,
error or retry schedule. A server without a telemetry source returns 404.
Responses must match the configured node identity, have finite nonnegative
rates/latency, a version no longer than 128 bytes, and a sample time within 60
seconds of the local clock. Peers need reasonably synchronized clocks.

Node views now return nullable `role`, `state`, `requests_per_minute` and
`open_streams`. Existing numeric/enum values keep their JSON representation.
New `telemetry_observed_at`, `telemetry_received_at` and `telemetry_stale` fields
separate the peer's sample time from this node's receipt time. A sample is fresh
for 15 seconds by both clocks. Missing or expired samples produce null metrics,
role, state and version; their last timestamps remain visible. Clients must
render null as unknown, including for peers running older versions, rather than
coercing it to zero or fallback. Replication reachability and sync lag remain
separate from telemetry availability.

Request rate is the number of runtime inference identifications in the trailing
60 seconds, including failed requests and WebSocket response.create frames.
Retries selected within the same request do not increment it. Startup reports
the observed count without extrapolation. Open streams reuse the live hook
counter. `added_latency_p50_ms` is the median SAP selection callback duration,
excluding upstream time. It is not a measurement of total proxy overhead. It
uses the latest 1024 selections within 60 seconds and is null before a selection
or after the samples expire. Aggregate stats expose the local selection latency
and RSS, while their existing request/usage series retain their current scope.

RSS is current resident memory, sampled by the node maintenance task once per
second, using `/proc/self/status` on Linux and `proc_pidinfo` on macOS. Failed or
unsupported reads return null. No subprocess or new dependency is needed.
The telemetry callback reads in-memory counters; RSS I/O happens outside it.
Direct calls to `Node` hooks bypass runtime instrumentation; embedding callers
should use the hooks installed on `Runtime` to collect rates and latency.

Validation uses local mocks and private auth directories. The sync tests cover
authentication, an older peer without samples, independent replication, node
identity rejection and response size limits. Node tests cover freshness expiry,
unknown versus measured zero, and telemetry across two loopback nodes with a
live stream and measured selection latency/RSS. No provider call or deployment
is part of this check.

Peer telemetry checks passed with `CARGO_BUILD_JOBS=2 nice -n 19` and subprocess
alarms of 300 seconds for tests/clippy and 60 seconds for formatting:

- `cargo test -p sap-sync -p sap-core -p sap-server`: 30 tests passed.
- `cargo clippy -p sap-sync -p sap-core -p sap-server --all-targets -- -D warnings`.
- `cargo fmt -p sap-core -p sap-sync -p sap-server --check`.
## Active refresh and classification validation, October 22, 2026

The active refresh changes touch `reset_aware.rs`, `quota.rs`, `hooks.rs`, the
executor usage-probe API, and upstream hook classification. Small `node.rs`
changes add refresh state and a weak self-reference so an HTTP caller dropping
its future does not cancel the shared refresh. `views.rs`, peer telemetry,
`sap-sync`, `sap-core`, packaging, and UI are unchanged.

Local checks use `CARGO_BUILD_JOBS=2 nice -n 19`, with subprocess alarms.

- `cargo fmt -p sap-server -p cpa-exec -p cpa-server -- --check`: passed.
- `cargo clippy -p sap-server -p cpa-exec -p cpa-server --all-targets -- -D warnings`: passed.
- `cargo test -p sap-server`: 5 API tests and 13 node-routing tests passed.
- `cargo test -p cpa-exec --lib usage_probe`: 3 probe tests passed.
- `cargo test -p cpa-server --lib sap_hooks`: 7 hook tests passed.

Mock tests use loopback upstreams and private auth directories. They cover
singleflight after caller cancellation, real polling status, observation sources,
sanitized errors and recovery, usage headers, redirect and wrong-provider refusal,
body-read timeout, Codex exhaustion recovery including stale thread failures,
and entitlement classification without account revocation.
No real provider, login, running proxy replacement, push, or deployment is part
of this validation. An earlier debug binary was staged under
`target/active-quota-install/bin/cliproxy`; its final rebuild was cancelled at
parent request. Parent integration owns the final binary build and local install.
