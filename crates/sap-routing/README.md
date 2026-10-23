# sap-routing

Implements Workstream B's pure thread-affinity policy. It performs no I/O, reads no
wall clock, and never mutates assignments. The server applies the returned decision.
Only `RoutingInput::now` determines time-dependent behavior, and
`RoutingInput::pick` supplies the randomness for weighted choice.

## Public API

```rust
ThreadAffinityPolicy::new(config: RoutingConfig) -> ThreadAffinityPolicy

// sap_core::routing::RoutingPolicy implementation:
fn decide(&self, input: &RoutingInput) -> Decision;
fn scores(&self, input: &RoutingInput) -> Vec<AccountScore>;

plan_capacity(plan: &str, cfg: &RoutingConfig) -> Option<f64>
```

Import `sap_core::routing::RoutingPolicy` to call the trait methods. Domain types
come from `sap-core`; no new dependencies or changes to that contract are needed.
Plan lookup is an ASCII case-insensitive substring search in table order. Routing
scales an account by its manual override, otherwise by its plan's capacity, otherwise
by 1. A stored value from the retired capacity learner is ignored.

## Scoring

Weights follow the weekly window. The five-hour window only tapers an account's
share near its limit.

```text
remaining = capacity * (1 - weekly_used_percent / 100)
U = remaining / max(seconds_to_weekly_reset, max(reset_floor_secs, 1))
H = remaining
L = active_threads / sum(active_threads of eligible accounts)
raw = urgency_weight * U/max(U) + headroom_weight * H/max(H) - load_weight * L
taper = 1 below five_hour_taper_start_percent (90), falling linearly to 0 at 100
share = max(raw, 0) * taper
```

Defaults are `urgency_weight` 1 and the other weights 0, so an account's share is
its weekly allowance left divided by its time to reset: the rate at which it must
be used so that none of it is wasted. Maxima are over eligible accounts; a zero
denominator contributes zero. A stale weekly window (missing, older than
`stale_after_secs`, or past its reset) multiplies the quota and load terms by
`stale_penalty`. A weekly window past its reset counts as a full allowance with a
full window to go. An unknown weekly reset contributes no urgency. The `stale`
flag also reports a missing or old five-hour window, without a penalty.

Drain: an eligible account whose weekly reset is within `drain_window_secs` (36
hours) and has allowance left takes every new thread. With several, the earliest
reset drains first. Its share is its five-hour taper; the remainder is split over
the other accounts in proportion to their shares, so a draining account hands over
gradually between 90 and 100 percent of its five-hour window. If nothing is
positive, eligible accounts get equal shares. Shares sum to 1 over eligible
accounts. `score` is the share and `weight_percent` is 100 times it.

New threads sample an account in proportion to its share using
`RoutingInput::pick`, a uniform number the server supplies. Accounts are ordered
heaviest first, then by higher priority, then by smaller account ID, so a pick near
zero takes the best account. Outcome-driven moves rescore with the failing account
excluded, so its share is redistributed.

The deciding `ScoreDetail` is `ResetSoon` for a draining account; otherwise it is
the largest positive weighted contribution advantage over the runner-up, with
`ResetSoon` for ties and five-hour hand-overs. A sole candidate is `OnlyChoice`.

## Decisions

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

Compaction moves only when `other > current + abs(current) * margin`, comparing
shares. A compacted thread therefore moves onto a draining account and off an
account whose five-hour taper has clearly handed over. Hard-ineligible current
accounts move immediately. Rejections report the earliest future reset or cooldown known among accounts of
the requested provider serving the model, including an exhaustion outcome's reset.
No unknown time is replaced by a guessed reset.

## Validation and remaining integration

Run only this crate, with command timeouts in the invoking harness:

```sh
CARGO_BUILD_JOBS=4 cargo fmt -p sap-routing
CARGO_BUILD_JOBS=4 cargo clippy -p sap-routing --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test -p sap-routing
```

Tests exercise every decision-table row, scoring edge cases, and plan lookup. They use in-memory contract objects and do not open a network connection
or read credentials. Provider/executor wiring, persistence, full-history resend,
and real-client mock-upstream integration belong to the integrating server.

Contract requests:

- Add the confirmed exhausted `WindowKind` to `AccountRuntime`. Until then,
  continuation labels use the highest observed percentage's kind, or
  `Other("unknown")`; `AfterOutcome` uses the exact supplied kind.
- Consider `ScoreDetail` variants for freshness and deterministic ties, and an
  explicit unknown-weight representation for the all-nonpositive case.

Known limits: five-hour and weekly are the expected windows because the contract
has no provider-specific required-window list. Defaults have not been calibrated against replayed production
usage. No provider behavior or cache sharing is established by these pure tests.
