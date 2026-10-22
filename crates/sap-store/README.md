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
record_peer_ack(peer: &NodeId, cursor: &SyncCursor) -> Result<()>
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

## Causal context and remaining contracts

Every operation carries `Change::context`, the origin's cursor when it made the
change. Set and Append values are ordinary domain JSON, with no private envelope.
An absent context deserializes as an empty cursor. All operations reject a context
that claims to have observed their own sequence or a later one. Concurrent
configuration deletes and field edits create conflicts regardless of delivery
order; the higher HLC determines whether the record remains deleted or revives.
Delete conflict sides use JSON null for the deleted value. Databases written by
the earlier private-envelope prototype have no upgrade migration.

`ChangeLog::record_peer_ack` delegates to the same durable, monotonic peer-cursor
storage as `set_peer_cursor`. The sync ACK handler calls it before returning
success, so accepted acknowledgements drive retention across restarts.

`changes_since` enumerates known origins from the cursor table and seeks through
the unique `(origin, seq)` index. Each origin contributes at most `limit + 1`
rows above the requested cursor, including origins missing from that cursor.
A fixed-width radix merge orders candidates by insertion ID in linear time;
only the selected page is deserialized. Historical rows below the cursor are
never scanned. `more` reports whether any candidates remain after the page.

Remaining integration contracts:

1. Define snapshot/bootstrap transfer and a typed history-unavailable response.
   After pruning, an old or new peer cannot replay from an empty cursor. The store
   reports that the cursor predates retained history rather than silently losing
   data. Snapshot export/import is not implemented by this workstream.
2. Define a core client-access record if that API needs a typed shape. This store
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

Known gaps are snapshot bootstrap, typed client-access
contracts, Windows ACL enforcement, and background integration. The initial
migration is implemented; there are no historical schema upgrades yet. Tests
cover local file and in-memory stores, not a distributed deployment or a soak.

## Validation

Validation runs offline with four build jobs and a 240-second timeout per build
or test invocation. The focused follow-up checks include sap-core, sap-store,
sap-sync, sap-threads and sap-routing:

```sh
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo fmt -p sap-core -p sap-store -p sap-sync
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo clippy -p sap-core -p sap-store -p sap-sync -p sap-threads -p sap-routing --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 CARGO_NET_OFFLINE=true cargo test -p sap-core -p sap-store -p sap-sync -p sap-threads -p sap-routing
```

Formatting, clippy with warnings denied, and all 56 tests across the five crates
passed. No tests were ignored.

Regressions cover delete/edit conflict ordering and causal edits, validation of
causal context for every operation, durable peer acknowledgements after reopen,
and a nearly current cursor over 10,000 historical rows. The tail regression
checks SQLite VM steps as well as avoiding historical JSON deserialization.

The integration suite covers two-node convergence and resolution, causal edits
and late ancestors, three-node conflict relay, append deduplication, cross-node
usage totals, thread tombstones and recreation, assignment divergence events,
settings/client-access merging, credential isolation, atomic rollback, sequence
gaps and pagination, remote clock observation, file persistence and 0600 sidecars,
concurrent independent writer handles, age retention, all-peer acknowledgements,
stale cursors, replay after pruning, byte pressure and inactive-thread eviction.
All tests use temporary local storage or in-memory SQLite; none contact a network
or load a provider credential directory.
