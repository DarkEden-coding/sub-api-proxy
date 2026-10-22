# Subscription API proxy discovery

Recorded October 22, 2026. This document records the user's decisions, a recommended project base, and facts that still require validation. Discovery is complete. Results of the client checks are in [validation.md](validation.md), and the build plan is in [implementation-plan.md](implementation-plan.md). Implementation has not started.

## Objective and scope

Build a private proxy for one person using multiple Claude and OpenAI subscription accounts from macOS and Linux. Codex CLI uses OpenAI accounts. Claude Code uses Claude accounts. Account counts and concurrency are not fixed product limits; resource use must be bounded and measured across increasing loads.

The main success criteria are fast server failover, reliable per-thread account affinity, better use of subscription allowances before reset, and low memory and processing overhead with large coding conversations.

The initial scope includes a CLI, a small web dashboard, and compatibility with the existing macOS usage visualizer. Cross-provider translation, automatic model substitution, a multi-user service, and local answer replay are outside the agreed initial scope.

## Deployment and failover

- Run a full server on Linux and on this Mac. Prefer Linux during normal operation. All clients fall back to this Mac when Linux is unavailable.
- Run a small background routing process on each client. Coding clients use a fixed localhost address. The routing process selects a server over Tailscale.
- Target switching a new request to a reachable backup in under five seconds, excluding upstream model response time. Test both immediate connection errors and silent network failures.
- Switch silently before output starts only where replay is safe. Lack of output alone does not establish that an upstream request was never accepted. Recovery must distinguish unsent requests, explicit rejection, ambiguous submission, and delivered output.
- If a server dies after output reaches a client, let the client retry the interrupted turn through the backup. Seamless transfer of an active provider connection is not a requirement.
- When Linux recovers, let it catch up and pass a stable health period, then send subsequent requests there. Let existing streams finish. Preserve the thread's subscription assignment when changing servers.
- Both servers may serve their reachable clients during a network split. Prefer availability and reconcile afterward.
- Use the proxy binaries, Tailscale, embedded storage, and direct peer sync. No separate database, coordinator, or always-on third machine is required.

The five-second target concerns failure detection and selection for new requests. It is not a timeout on model reasoning, a promise to finish inference in five seconds, or a guarantee when neither server is reachable.

## Account selection and cache reuse

The final affinity rule is per thread. The user's final clarification supersedes the earlier group-wide pinning answer.

| Event | Required behavior |
| --- | --- |
| New independent thread | Choose an eligible account using quota remaining, reset times, and current load. |
| Child agent or split thread starts | Prefer its parent's current account when eligible. Establish a separate assignment for the new thread. |
| Ordinary continuation | Keep the assigned account. |
| Temporary rate limit or overload | Retry the same account within a configurable deadline. If it remains unavailable, return a retryable error and preserve the assignment. |
| Confirmed 5-hour or weekly exhaustion | Move to another account supporting the requested model. Keep the new assignment. Cache loss is acceptable. |
| Revoked, disabled, or model-ineligible account | Move to another eligible account serving the same requested model. |
| Successful compaction | Reconsider that thread's assignment. Move only when quota, reset time, or load makes another account meaningfully better. Other threads keep their assignments. |
| All eligible accounts exhausted | Return a clear retryable error and the earliest known usable reset time. Do not invent a reset time if it is unknown. |

Routing should account for all reported limiting windows and model entitlements. Different plan capacities must not be treated as equal merely because they report the same remaining percentage. Missing or stale quota data must be represented explicitly.

Caching means preserving provider prompt-cache reuse through account affinity, stable request prefixes, and preserved cache controls. It does not mean returning a saved model answer. Preserve native client behavior, including tool calls, streaming, thinking or reasoning fields, images, and compaction where available to the account.

Provider cache hits remain an observed outcome. Keeping an account and prefix stable does not establish that separate server logins share a provider cache. Measure cache-read and cache-write usage across continuations, node changes, child starts, compaction, and quota-driven moves.

## State, recovery, and synchronization

Use stable logical account identities across servers, with separate provider-issued login sessions on each server if provider behavior confirms that they refresh independently. Keep node-owned credentials separate from replicated logical account records. Import existing formats where practical without assuming a copied refresh token can be safely used by two servers.

Synchronize account metadata and controls, client access configuration, routing settings, thread assignments and lineage, compaction generations, quota observations, relevant usage history, and bounded recovery state. Distinguish each node's local connection health and credentials from shared settings.

Full parity means equivalent serving capability and converged shared state when the nodes can communicate. During a split, routing choices, quota observations, and edits may diverge. Live sockets and inference already running upstream are not replicated state.

Allow administration on either node during a split. Merge independent changes after reconnecting. Show conflicting changes that require a choice instead of silently overwriting them. The exact versioning and reconciliation algorithm remains an implementation decision that must preserve these behaviors.

Keep a small, configurable memory budget for recovery data. Permit a separate, larger configurable disk budget so memory remains bounded. The user permits unencrypted conversation recovery data. Use restricted local file permissions, exclude secrets from logs, and authenticate peer and management operations.

Do not persist full transcripts by default. Store only context needed for routing and recovery, with configurable retention. If both memory and disk budgets are exhausted, do not silently omit conversation context. Recover from client-supplied full context where supported or return an explicit recoverable error. The exact eviction and overload behavior should be documented with the chosen storage design.

Recovery data is separate from the provider's prompt cache. A local snapshot cannot reconstruct a provider's internal cache or automatically resume a lost socket.

## Client integration and management

Keep Codex CLI and Claude Code unmodified. Use supported client configuration, hooks, or a launch wrapper when ordinary API metadata does not reliably identify threads, parents, forks, or compaction. Do not infer compaction solely from a smaller request body.

Both clients currently document compaction and subagent lifecycle hooks. Hook events must still be correlated with actual request identifiers. Codex documents parent session IDs in subagent hook input, which makes the distinction between parent ID and child ID particularly important. Installed versions must be checked during integration. [4][5]

Provide account login and controls, configuration, diagnostics, and serving-node visibility through the CLI and small web dashboard. Show account windows and reset times, routing reasons, thread assignments, cache usage when available, node health, failovers, sync lag, and conflicts. Keep the dashboard within the private deployment.

The reference app is the local `CLI-Proxy-Usage-Indicator` project (a sibling of this checkout). Its expected management routes are:

- `GET /v8/management/routing/reset-aware/status`
- `GET /v8/management/routing/reset-aware/history?hours=3`
- `GET /v8/management/routing/reset-aware/requests`
- `POST /v8/management/routing/reset-aware/refresh`

Preserve the response contracts read by its `UsageCore/Models.swift`, including provider names `claude` and `codex`, account routing state, weights, shares, quota windows, history samples, and request buckets. Ensure the displayed routing decisions reflect the actual selector. The current app also calculates ranking locally, so changed routing semantics may require an agreed app adjustment to keep its explanations accurate.

The existing server's custom source is unavailable. Reconstruct compatibility from the viewer and its tests; do not claim to reproduce the unavailable selector exactly. The viewer currently points to one server hostname, so its connection handling will also need a stable failover path if its own availability should match inference clients.

## Recommended project base

Recommend a focused Rust fork of `vayungodara/cliproxy-rs`, conditional on validation of the required native paths. Reuse its provider integration and compatibility work. Add the local routing process, peer synchronization, and the specified routing policy as clear modules. Retain an upstream remote and a recorded base revision so relevant fixes remain traceable.

| Candidate | Assessment |
| --- | --- |
| `vayungodara/cliproxy-rs` | Preferred base. Existing native Claude and Codex paths, credential/config compatibility, management routes, and a documented parity audit provide useful reuse. Its audit is incomplete and does not prove live-provider behavior. [1] |
| `IuCC123/CLIProxyAPI-Rust` | Credible alternative with affinity and quota-aware selection already implemented. The inspected code keeps continuation history in process memory, clones parsed conversation structures, and persists assignments through snapshot writes. Those areas would need work for low-memory replicated recovery. [3] |
| Go CLIProxyAPI | Keep as the behavior reference and fallback base. Rust should win through measured resource use and maintainability, rather than language preference alone. |
| Entirely new proxy | Avoid recreating authentication, native streaming, and continuation handling before testing existing code. A new focused local router and peer-sync implementation are still needed for this deployment. |

The preferred Rust port publishes repeatable mock-upstream benchmarks and large-prompt soak investigations. Its small-request idle measurements do not establish memory use for long coding sessions. Published field observations include much higher memory use, so benchmark the selected revision and exact request paths instead of adopting a README number as a target. [2]

Source snapshots inspected locally:

- `cliproxy-rs`: `85c7cf3`
- `CLIProxyAPI-Rust`: `b07936b`
- Go CLIProxyAPI: `ceb045a`

These are review snapshots, not a finalized dependency selection or a measured performance comparison.

## Validation required before relying on the design

1. Confirm separately issued sessions for the same subscription remain valid when each node refreshes independently, including during a network split. Official OpenAI guidance requires serializing refreshes for a shared session; independent issuance must be verified rather than assumed. [7]
2. Run both clients through native provider paths with ordinary turns, tool calls, reasoning, long prompts, images where supported, compaction, and resumed threads. Verify parent/child/fork identifiers against actual HTTP and WebSocket requests.
3. Measure provider cache usage when the same logical account serves requests from separate node logins. Preserve necessary request content and account affinity through node changes.
4. Exercise process crashes, refused connections, network black holes, upstream-only failures, active-stream interruption, recovery, and failback. Verify the under-five-second target for new requests without treating a healthy server waiting on inference as dead.
5. Test concurrent edits and routing changes during a split, stale quota samples, account disablement, and reconnect reconciliation. Count usage observations without double counting mirrored records.
6. Test bounded memory and disk behavior, slow clients, request cancellation, backpressure, growing prompts, and long-running sessions. Record idle and peak RSS, retained RSS after load, CPU, and proxy-added latency across increasing accounts and concurrency on macOS and Linux.
7. Verify the visualizer's response contracts, displayed routing reasons, and connection path during failover.

Tailscale is the networking layer. Its documented default route failover can take about 15 seconds on a clean disconnect and longer on partitions, so it is insufficient evidence for the application's five-second goal. The local routing process must implement and test its own application-health and server-selection behavior. [6]

No exact RSS ceiling or proxy-added latency budget has been agreed. Set initial budgets from realistic baseline measurements, keep them configurable where appropriate, and report regressions against the same workloads. Do not introduce fixed account-count ceilings as substitutes for bounded resource management.

## Sources

Primary sources inspected during discovery. URLs are recorded for future verification because upstream code and documentation can change.

1. cliproxy-rs parity audit: `https://github.com/vayungodara/cliproxy-rs/blob/master/docs/PARITY.md`
2. cliproxy-rs benchmarks: `https://github.com/vayungodara/cliproxy-rs/blob/master/docs/BENCHMARKS.md`
3. Alternative Rust source: `https://github.com/IuCC123/CLIProxyAPI-Rust/blob/main/src/affinity.rs`, `https://github.com/IuCC123/CLIProxyAPI-Rust/blob/main/src/proxy.rs`
4. Official Codex hooks documentation: `https://learn.chatgpt.com/docs/hooks`
5. Official Claude Code hooks documentation: `https://code.claude.com/docs/en/hooks`
6. Tailscale route selection: `https://tailscale.com/docs/features/route-selection`
7. Official OpenAI account/session and refresh guidance: `https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions`
8. cliproxy-rs migration and refresh warning: `https://github.com/vayungodara/cliproxy-rs/blob/master/docs/MIGRATING-FROM-GO.md`

Local reference: the `CLI-Proxy-Usage-Indicator` README and its `Sources/UsageCore` and `Sources/CLIProxyUsage/ManagementClient.swift` files.
