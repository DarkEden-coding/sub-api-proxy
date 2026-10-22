# sap-sync

HTTP pull replication over Tailscale, generic over `sap_core::sync::ChangeLog`.
No `sap-store` dependency. Storage merges, deduplication, and contiguous per-origin
cursors belong to the supplied log.

## Public API

```rust
// C: ChangeLog + 'static throughout; all types are exported at crate root.
ServerState::<C>::new(
    log: Arc<C>,
    secrets: BTreeMap<NodeId, Vec<u8>>,
    notify: Arc<Notify>,
) -> ServerState<C>
ServerState::<C>::allow_loopback(self) -> Self
ServerState::<C>::acknowledgements(&self) -> BTreeMap<NodeId, SyncCursor>
router<C: ChangeLog + 'static>(state: Arc<ServerState<C>>) -> axum::Router

Peer::new(node: NodeId, base_url: &str, secret: Vec<u8>) -> Result<Peer, String>
SyncAgent::spawn<C: ChangeLog + 'static>(
    log: Arc<C>, peers: Vec<Peer>, notify: Arc<Notify>,
) -> SyncAgent
SyncAgent::statuses(&self) -> BTreeMap<NodeId, PeerStatus>
SyncAgent::shutdown(self) // async
```

`Peer` is cloneable and exposes `pub node: NodeId`. Secrets have no Debug output.
`PeerStatus` implements Clone, Debug, Default, and Serialize:

```rust
pub struct PeerStatus {
    pub last_success: Option<u64>, // Unix seconds, after apply and ACK succeed
    pub last_error: Option<String>, // sanitized transport or storage category
    pub lag_secs: Option<u64>,
    pub cursor: SyncCursor, // local applied cursor at the last successful exchange
}
```

Lag is sampled from the newest applied HLC while catching up and becomes zero
when `more` is false. It is not a live measure of a disconnected peer's backlog.
Check `last_success` and `last_error` alongside it.

Share one `Arc<Notify>` between the server, agent, and local writers. Writers must
call `notify.notify_waiters()` after committing, so every peer's long poll wakes.
Remote applies also notify waiters. Synchronous log methods run on blocking
workers and must return promptly. Their work cannot be cancelled once started.

Serve the router with actual socket connection metadata:

```rust,ignore
axum::serve(listener, router(state)
    .into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
```

Connections without metadata are rejected. Default inbound policy accepts only
`100.64.0.0/10` and `fd7a:115c:a1e0::/48`, including mapped IPv4. Loopback requires
`ServerState::allow_loopback()`. Forwarded headers are never used for authorization.
Outbound peer URLs require literal Tailscale or loopback IP addresses, with optional
ports. DNS names, credentials, path prefixes, queries, fragments, empty secrets,
and non-HTTP schemes are rejected. HTTP relies on Tailscale's transport encryption.
Configure each pair with its own shared secret; the inbound map is keyed by the
caller's node identity, while `Peer::node` identifies the remote node.

## Wire protocol

- `GET /sap/sync/changes?since=<percent-encoded JSON SyncCursor>&limit=N&wait_ms=M`
  returns JSON `sap_core::sync::ChangeBatch`. `since` is required. `limit` defaults
  to 1000 and is capped at 1000; zero is rejected. `wait_ms` defaults to zero and
  is capped at 25000. Existing changes return immediately; otherwise the handler
  waits for notification or the deadline and reads the log again. `more: true`
  causes an immediate next pull.
- `POST /sap/sync/ack` accepts a bare JSON `SyncCursor`, such as `{"macbook":12}`,
  with `content-type: application/json`. Success is `204`. Each authenticated
  peer's stored acknowledgement advances monotonically per origin.

Both requests require:

```text
x-sap-peer: <caller's NodeId string>
x-sap-timestamp: <Unix seconds, decimal>
x-sap-signature: <lowercase hex HMAC-SHA256>
```

The signature input is exactly the following UTF-8 string, with three newline
bytes and no final newline:

```text
METHOD\nPATH_AND_QUERY\nTIMESTAMP\nLOWERCASE_HEX_SHA256_BODY
```

`METHOD` is the HTTP method as sent. `PATH_AND_QUERY` includes the exact encoded
query, including its order. The timestamp is the exact header string. The body
hash covers the exact bytes, including JSON whitespace. GET uses an empty body.
Signatures are checked in constant time. Skew greater than 60 seconds in either
direction is rejected. This permits replay within the skew window; GET and ACK
are idempotent. Responses are not signed and rely on the private network.

Authentication failures return `401`, address failures `403`, malformed queries
or a zero limit `400`, storage failures `500`, oversized bodies or an individually
oversized change `413`, and slow request bodies `408`. Bodies are capped at 8 MiB,
including batch envelopes. Large batches are split at a change boundary with
`more: true`. A single change that cannot fit requires operator intervention or
a future chunked protocol.

The client caps response bodies at 8 MiB, connects within 3 seconds, and times
out each HTTP request after 30 seconds. It rejects unexpected batch identities,
invalid JSON, oversized batch counts, and empty/no-progress catch-up batches.
Retries back off from 100 ms to 5 seconds, resetting after success. A 10 ms floor
between completed polls bounds immediate-empty-response loops. Each peer has an
independent task; dropping or shutting down the agent aborts those tasks.

## Validation

Only localhost sockets and an in-memory `ChangeLog` double are used. Every async
test has a 4 or 8 second overall timeout. Build commands were run with a 240 second
process timeout and `CARGO_BUILD_JOBS=4`:

```sh
CARGO_BUILD_JOBS=4 cargo fmt -p sap-sync
CARGO_BUILD_JOBS=4 cargo clippy -p sap-sync --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test -p sap-sync
```

Coverage includes bidirectional convergence within one second, ACK recording,
server partition/reconnect without lost changes, bad HMAC, timestamp skew, query
and body tampering, simultaneous long-poll wakeups and expiry, response-size
pagination, oversized-record rejection, destination validation, and default
loopback denial.

## Contract requests and gaps

- Add a durable `ChangeLog::set_peer_cursor(peer: &NodeId, cursor: &SyncCursor)`
  operation or an equivalent ACK sink. ACKs currently live only in server memory;
  `acknowledgements()` lets the integrator inspect them, but they do not drive
  `sap-store` retention automatically.
- Add a shared `sap_core::sync::PeerStatus` if status crosses another crate's
  public boundary. The current transport-local type fills the missing contract.
- A remote high-water cursor/timestamp in `ChangeBatch` would support more accurate
  lag estimates. Current lag is a sample, not measured backlog during partitions.
- Production server wiring, real `sap-store` integration, and Nodes page mapping
  remain integration work outside this crate. No provider traffic was sent.
