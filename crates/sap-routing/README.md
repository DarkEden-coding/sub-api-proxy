# sap-routing

Implements Workstream B's pure thread-affinity policy. It performs no I/O, reads no
wall clock, and never mutates assignments. The server applies the returned decision.
Only `RoutingInput::now` determines time-dependent behavior.

## Public API

```rust
ThreadAffinityPolicy::new(config: RoutingConfig) -> ThreadAffinityPolicy

// sap_core::routing::RoutingPolicy implementation:
fn decide(&self, input: &RoutingInput) -> Decision;
fn scores(&self, input: &RoutingInput) -> Vec<AccountScore>;

plan_capacity(plan: &str, cfg: &RoutingConfig) -> Option<f64>

CapacityLearner::new(
    alpha: f64,
    min_samples: usize,
    baseline_tokens_per_percent: f64,
) -> Result<CapacityLearner, &'static str>

CapacityLearner::observe(
    &mut self,
    account: &AccountId,
    window: &QuotaWindow,
    cumulative_tokens: u64,
) -> Option<Capacity>

CapacityLearner::forget_account(&mut self, account: &AccountId)
```

Import `sap_core::routing::RoutingPolicy` to call the trait methods. Domain types
come from `sap-core`; no new dependencies or changes to that contract are needed.
Plan lookup is an ASCII case-insensitive substring search in table order. Routing
uses the account's resolved capacity, so the caller applies plan discovery, learning
and manual override precedence before constructing its input.

## Scoring

For every valid reported window:

```text
remaining_w = capacity.multiplier * (1 - used_percent_w / 100)
urgency_w = remaining_w / max(seconds_to_reset_w, max(reset_floor_secs, 1))
U = sum(urgency_w)
H = min(remaining_w)
L = active_threads / sum(active_threads of eligible accounts)
score = urgency_weight * U/max(U) + headroom_weight * H/max(H) - load_weight * L
score *= stale_penalty, once, if any window is stale or missing
```

Both maxima are over eligible accounts. A zero denominator contributes zero.
An unknown reset contributes zero urgency without inventing a reset. An account
with no usable window observations contributes zero quota terms, rather than
assuming zero usage. Missing five-hour or weekly observations, invalid percentages,
or observations older than `stale_after_secs` mark the account stale. Additional
reported windows also participate. Stale observations retain their measured usage.
A reset in the past uses the configured urgency floor; this policy does not infer
new usage after a reset.

Scores use active threads, as required by the workstream brief, rather than the
older implementation plan's active streams. Negative scores remain negative.
Positive scores determine proportional display weights. If none are positive,
eligible accounts get equal display weights to satisfy the contract's sum of 100.
Ineligible accounts get zero score and weight, with an explanation. Score ties
prefer higher priority, then lexicographically smaller account ID.

The deciding `ScoreDetail` is the largest weighted contribution advantage over the
runner-up, including the stale multiplier. Exact ties use `ResetSoon`; the current
contract has no priority, ID-tie, or freshness detail. A sole candidate is `OnlyChoice`.

## Decisions and learning

New threads select the best score; children prefer an eligible parent. Cooling
blocks new assignments but does not break existing affinity. Continuations stay
unless their account is hard-ineligible. Unknown exhaustion resets remain
ineligible. Outcome-driven moves exclude the failing account even if the caller
has not yet updated its runtime state.

Rate limits and overloads retry the same assignment while `now` is before the
failure deadline. Retry time is the later of the outcome and runtime cooldown hints,
or `now` when neither has a future hint. At the deadline the policy rejects without
moving. The caller must enforce the deadline and re-evaluate before sending a retry,
including when a provider's retry hint extends beyond the deadline.

Compaction moves only when `other > current + abs(current) * margin`. This agrees
with the relative margin for positive scores and requires a positive improvement
for zero or negative scores. Hard-ineligible current accounts move immediately.
Rejections report the earliest future reset or cooldown known among accounts of
the requested provider serving the model, including an exhaustion outcome's reset.
No unknown time is replaced by a guessed reset.

Learning is isolated by account and window kind. Each positive sample is
`delta_cumulative_tokens / delta_used_percent`, followed by
`EWMA = alpha * sample + (1 - alpha) * previous_EWMA`. After `min_samples`, the
capacity multiplier is `EWMA / baseline_tokens_per_percent`, with source `Learned`.
Unchanged percentages accumulate token deltas until the next percentage increase.
Duplicate and older observations are ignored. Reset changes, usage decreases, and
token counter restarts establish a new baseline without discarding the learned
estimate. Unknown resets and invalid samples never train the learner.

## Validation and remaining integration

Run only this crate, with command timeouts in the invoking harness:

```sh
CARGO_BUILD_JOBS=4 cargo fmt -p sap-routing
CARGO_BUILD_JOBS=4 cargo clippy -p sap-routing --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test -p sap-routing
```

Tests exercise every decision-table row, scoring edge cases, plan lookup, and
learning. They use in-memory contract objects and do not open a network connection
or read credentials. Provider/executor wiring, persistence, full-history resend,
and real-client mock-upstream integration belong to the integrating server.

Contract requests:

- Add the confirmed exhausted `WindowKind` to `AccountRuntime`. Until then,
  continuation labels use the highest observed percentage's kind, or
  `Other("unknown")`; `AfterOutcome` uses the exact supplied kind.
- Add learner tunables and a documented account-scoped cumulative-token counter
  convention. The caller currently supplies these through the API above. The
  learner returns per-window estimates; the caller chooses which to apply.
- Consider `ScoreDetail` variants for freshness and deterministic ties, and an
  explicit unknown-weight representation for the all-nonpositive case.

Known limits: five-hour and weekly are the expected windows because the contract
has no provider-specific required-window list. The learner keeps one entry per
observed account/window pair; callers must remove deleted accounts with
`forget_account`. Defaults have not been calibrated against replayed production
usage. No provider behavior or cache sharing is established by these pure tests.
