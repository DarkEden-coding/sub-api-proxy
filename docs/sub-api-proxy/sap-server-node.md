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
idle_window_secs: 600
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
are `codex:<account_id>`. Credentials without these subscription identity fields
are not invented into logical accounts. Account preferences survive rescans;
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
- 401/403 produce local `Revoked`, then `Use` moves to another account.
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
- Active quota probing is not implemented. `reset_aware_refresh` returns the
  latest passive status and `polling` is false. The executors need a public,
  test-guarded usage-probe API before this can meet Control's refresh promise.
- `SapPick` cannot express delayed selection or `Retry-After` headers. Future
  retry times therefore use 429 plus body `retry_at`, rather than waiting inside
  synchronous `select`.
- G's `CloseSocket` contract completes the current response using configured
  selection before closing. It does not close before dispatch or pin that final
  attempt to the newly assigned account. This behavior is inherited, not changed
  here. Real Codex reconnect/full-history recovery remains unverified.
- There is no hook error kind identifying model entitlement failures. Explicit
  `ModelIneligible` classification needs a typed upstream signal; model changes
  already use `Account::serves_model` through the routing policy.
- The hook has no attempt ID. Usage deduplication is per request/credential;
  multiple cumulative counters restarted for the same credential within one
  request cannot be distinguished reliably. An attempt ID is needed for that case.
- Peer roles/remote stream counts and request rates are not provided by sync;
  peer roles display as fallback, remote streams/rates as zero. Latency and RSS
  remain absent. Health upstream flags use successful traffic from the last five
  minutes, without synthetic probes.
- Learner parameters are fixed starting values pending replay calibration.
  Additional Codex quota families are not parsed beyond primary/secondary.
- OAuth URL generation reuses management, but browser completion was not tested.
  Existing management availability/auth requirements still apply. A re-login
  validates the requested account's provider but cannot force the browser user
  to choose that same subscription.
- Workstream I must mount its management/dashboard router alongside the sync
  routes. This branch intentionally does not implement or edit `api/`.
