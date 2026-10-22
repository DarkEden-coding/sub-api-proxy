# sap-store

SQLite storage for Workstream C. All domain writes and remote batches use an
IMMEDIATE transaction. A write commits its domain row, HLC, origin sequence,
field versions, cursor and op-log entry together. Credentials use the same
transaction boundary but never allocate a sequence or enter the op log.

## Public API

All methods take `&self` unless shown otherwise. `Result<T>` is
`std::result::Result<T, sap_store::Error>`. Identifiers, records and synchronization
types below come from `sap_core`; `Value` is `serde_json::Value`.

```rust
Store::open(path: impl AsRef<Path>, node: NodeId, limits: Limits) -> Result<Store>
Store::open_in_memory(node: NodeId) -> Result<Store>

upsert_account(account: Account) -> Result<()>
patch_account(id: &AccountId, changes: &BTreeMap<String, Value>) -> Result<()>
accounts() -> Result<Vec<Account>>
put_thread(thread: ThreadRecord) -> Result<()>
threads() -> Result<Vec<ThreadRecord>>
delete_thread(key: &ThreadKey) -> Result<()>
append_quota(observation: QuotaObservation) -> Result<QuotaObservation>
latest_windows(account: &AccountId) -> Result<Vec<QuotaWindow>>
quota_history(account: &AccountId, since: DateTime<Utc>) -> Result<Vec<QuotaObservation>>
add_usage(account: &AccountId, bucket_start: DateTime<Utc>, deltas: ThreadUsage) -> Result<()>
usage_buckets(account: &AccountId, since: DateTime<Utc>)
    -> Result<BTreeMap<DateTime<Utc>, ThreadUsage>>
append_event(event: EventView) -> Result<()>
events(limit: usize) -> Result<Vec<EventView>>
conflicts() -> Result<Vec<Conflict>>
resolve_conflict(id: &str, keep: NodeId) -> Result<Conflict>
get_setting(key: &str) -> Result<Option<Value>>
set_setting(key: &str, value: Value) -> Result<()>
client_access(key: &str) -> Result<Option<Value>>
set_client_access(key: &str, value: Value) -> Result<()>
put_credential(credential: NodeCredential) -> Result<()>
credentials() -> Result<Vec<NodeCredential>>
set_peer_cursor(peer: NodeId, value: &SyncCursor) -> Result<()>
peer_cursors() -> Result<BTreeMap<NodeId, SyncCursor>>
compact() -> Result<CompactionReport>
compact_at(now: DateTime<Utc>) -> Result<CompactionReport>

// impl sap_core::sync::ChangeLog for Store, type Error = sap_store::Error
node() -> &NodeId
cursor() -> Result<SyncCursor>
changes_since(since: &SyncCursor, limit: usize) -> Result<ChangeBatch>
apply_remote(changes: &[Change]) -> Result<ApplyReport>
```

`Limits` has public `Duration` fields `quota_max_age`, `usage_max_age`,
`event_max_age`, `op_log_max_age`, `thread_max_age`, and `max_bytes: u64`.
Defaults are 14, 90, 7, 14 and 30 days, and 256 MiB respectively.
`open_in_memory` uses these defaults.

`CompactionReport` has public `append_rows_deleted: usize`,
`op_log_rows_deleted: usize`, `threads_deleted: usize`, `retained_bytes: u64`,
and `budget_met: bool`. `retained_bytes` includes the database pages and current
WAL/SHM files after VACUUM and checkpoint. In-memory stores report allocated
SQLite page bytes.

Account patches validate the resulting record, reject unknown fields and changes
to `id`, and commit atomically. Upserts emit only changed fields after creation.
A setting key holds any JSON value and merges as a single field named `value`.
Client-access records are JSON objects; supplied fields merge independently,
omitted fields stay unchanged, and unchanged fields do not create writes.
Quota appends assign the store's node and fresh sequence, replacing the input
identity and returning the assigned observation. Latest quota windows compare
observation time, then origin and sequence for deterministic ties.
Usage stores immutable per-node deltas and sums them into the caller's buckets;
callers choose bucket boundaries. Events return newest first. Conflicts include
resolved records. Repeating a resolution is a no-op. Credentials must belong to
the local node.

## Schema version 1

`PRAGMA user_version` controls transactional initialization. Newer versions are
rejected. Opening a database with another node identity is rejected.

- `accounts`, `settings`, `client_access`, `threads`, `conflicts` each have
  `key TEXT PRIMARY KEY, value TEXT NOT NULL`. Values contain JSON core records,
  except the untyped setting/client-access objects.
- `quota_observations`, `usage_buckets`, `events` have `origin TEXT`, `seq INTEGER`,
  `at_ms INTEGER`, `account TEXT`, `value TEXT`, primary key `(origin, seq)`, and
  indexes on time and on `(account, at_ms)`.
- `field_versions` has `(table_name, key, field)` as its primary key and JSON
  `value` containing the winning change, its causal context and field value.
  Empty `field` denotes a whole-record version or tombstone.
- `op_log` has insertion-ordered `id INTEGER PRIMARY KEY`, `origin`, `seq`,
  `wall_ms`, JSON `value`, and unique `(origin, seq)`.
- `cursor` has `origin TEXT PRIMARY KEY, seq INTEGER`.
- `peer_cursors` has `peer TEXT PRIMARY KEY, value TEXT` containing its cursor.
- `node_credentials` has `account TEXT PRIMARY KEY, value TEXT`. It is local only.
- `metadata` has `key TEXT PRIMARY KEY, value TEXT`. Entries hold the database's
  node, HLC and pruned-history floor.

Files use WAL and synchronous FULL, with a five-second SQLite busy timeout.
On Unix the database is created or tightened to 0600 before SQLite opens it;
SQLite creates its WAL/SHM sidecars with the database permissions. The tests check
all three files. Portable Windows ACL hardening is not implemented.

## Concurrency and conflict handling

A mutex serializes one handle's connection. SQLite's IMMEDIATE writer transaction
also serializes independent handles and processes using the same database, so
sequence allocation and read-modify-write operations cannot race.

Accounts and client-access configuration merge per field. Each setting key is one
field. The higher HLC wins provisionally, including its node tie break. For two
writes from different origins, a conflict is created when neither sender's cursor
includes the other write and their values differ. Checking both contexts avoids
false conflicts when an ancestor arrives after its descendant. Conflict IDs encode
the field and sorted origin/sequence pair, and detection times are deterministic.
Conflict creation and resolution are replicated too. An unresolved creation cannot
reopen a resolved conflict. Thread records use whole-record HLC ordering;
concurrent differing assignments generate ordinary replicated events instead of
conflicts. Deletes retain whole-record tombstones, and older sets cannot resurrect
threads. A later full record can recreate one. A configuration field newer than
a tombstone revives its record with the prior unedited fields, independent of
delete/set delivery order.

Remote batches are atomic, duplicates are ignored, and accepted remote HLCs
advance the persistent local clock. Sequences must arrive without gaps per origin;
otherwise the batch rolls back. Interleaving different origins is allowed, including
receiving a descendant before a causal dependency from another origin. Export
preserves local insertion order and paginates without skipping an origin's sequence.

## Causal envelope and contract requests

The core `Change` type lacks a sender cursor. To implement causal conflict
detection without editing sap-core, this store wraps Set values for accounts,
settings, client access and threads as:

```json
{"sap_store_version":1,"context":{"node-a":12},"value":"original JSON value"}
```

Sync transports must preserve these values unchanged. Whole account/thread
records use the same wrapper. Append values remain ordinary domain JSON.
Unwrapped remote Sets are accepted with an unknown, empty causal context, so they
can cause conservative conflicts. Delete has no value and cannot carry context;
it is handled as an HLC tombstone without generating configuration conflicts.

Requested integration contracts:

1. Add `context: SyncCursor` to `sap_core::sync::Change`, including Delete, and
   retire the private envelope once all peers support that representation.
2. Add peer-ack recording to `ChangeLog`, or have sap-sync's ACK route explicitly
   call `Store::set_peer_cursor`. Its existing trait has no ACK hook.
3. Define snapshot/bootstrap transfer and a typed history-unavailable response.
   After pruning, an old or new peer cannot replay from an empty cursor. The store
   reports that the cursor predates retained history rather than silently losing
   data. Snapshot export/import is not implemented by this workstream.
4. Define a core client-access record if that API needs a typed shape. This store
   uses JSON objects until that contract exists.

The server must wake its sync notifier after successful writes. Store does not
add a Tokio dependency or own a background scheduler.

## Retention and limits

`compact` removes expired append rows and emits tombstones for old threads with
no open streams. Under pressure it removes the oldest remaining append rows and
acknowledged log entries, then the oldest inactive threads. Log entries are
eligible only when every registered peer acknowledges their origin sequence.
Register all configured peers, including peers with empty cursors, before
compaction. With no registered peers the log is conservatively pinned.
Acknowledgements only advance; future acknowledgements of the local origin are
rejected. A peer may already know other origins that this node has not pulled. Cursors and pruned-history floors survive compaction, preventing replay
from double counting or restoring locally expired data.

Compaction VACUUMs the database and checkpoints/truncates WAL. Safety state,
credentials, accounts, settings, conflicts, active threads and unacknowledged
changes are never thrown away to force the byte budget to fit. If those rows,
the minimum schema size, or a busy WAL reader prevent reaching the limit,
`budget_met` is false. A VACUUM may need temporary disk space and blocks writers;
run compaction from maintenance work rather than a request handler. Thread
registry eviction in other crates is outside this store's scope.

Known gaps are snapshot bootstrap, typed causal metadata and client-access
contracts, Windows ACL enforcement, and background integration. The initial
migration is implemented; there are no historical schema upgrades yet. Tests
cover local file and in-memory stores, not a distributed deployment or a soak.

## Validation

Only sap-store and its dependencies were built, offline, with four build jobs.
Each Cargo invocation was bounded by a 180-second subprocess timeout, except the
first compilation, which had a 240-second timeout.

```sh
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo fmt -p sap-store
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo clippy -p sap-store --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo test -p sap-store
```

Final validation passed formatting, clippy with warnings denied, and all 15
integration tests. There are no ignored tests.

The integration suite covers two-node convergence and resolution, causal edits
and late ancestors, three-node conflict relay, append deduplication, cross-node
usage totals, thread tombstones and recreation, assignment divergence events,
settings/client-access merging, credential isolation, atomic rollback, sequence
gaps and pagination, remote clock observation, file persistence and 0600 sidecars,
concurrent independent writer handles, age retention, all-peer acknowledgements,
stale cursors, replay after pruning, byte pressure and inactive-thread eviction.
All tests use temporary local storage or in-memory SQLite; none contact a network
or load a provider credential directory.
