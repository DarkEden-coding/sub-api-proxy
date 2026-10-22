# sap-threads

Extracts request identity and maintains bounded thread records using `sap-core` types.
No networking, credentials, transcript storage, or body rewriting.

```rust
pub fn identify(path: &str, headers: &http::HeaderMap, body: &[u8]) -> Option<ThreadIdentity>;
pub fn identify_claude(headers: &http::HeaderMap, body: &[u8]) -> Option<ThreadIdentity>;
pub fn identify_codex(headers: &http::HeaderMap, body: &[u8]) -> Option<ThreadIdentity>;

pub struct Observation {
    pub record: Option<ThreadRecord>,
    pub is_new: bool,
    pub lineage: LineageKind,
    pub parent: Option<ThreadKey>,
}

impl Registry {
    pub fn new(max_records: usize) -> Self;
    pub fn observe(&mut self, identity: &ThreadIdentity, now: DateTime<Utc>, node: NodeId) -> Observation;
    pub fn get(&self, key: &ThreadKey) -> Option<&ThreadRecord>;
    pub fn record_response(&mut self, key: &ThreadKey, response_id: impl Into<String>) -> bool;
    pub fn record_usage(&mut self, key: &ThreadKey, delta: ThreadUsage) -> bool;
    pub fn stream_opened(&mut self, key: &ThreadKey) -> bool;
    pub fn stream_closed(&mut self, key: &ThreadKey) -> bool;
    pub fn compaction_completed(&mut self, key: &ThreadKey) -> Option<u32>;
    pub fn assign(&mut self, key: &ThreadKey, assignment: Assignment) -> bool;
    pub fn active(&self, now: DateTime<Utc>, idle_window: std::time::Duration) -> Vec<ThreadRecord>;
    pub fn children(&self, key: &ThreadKey) -> Vec<ThreadRecord>;
    pub fn export(&self) -> Vec<ThreadRecord>;
    pub fn import(&mut self, records: Vec<ThreadRecord>);
}
```

The dispatcher accepts `/v1/messages`, `/v1/responses`, `/responses`, and
`/backend-api/codex/responses`. Pass a path without its query string. Codex WebSocket
frame metadata overrides upgrade metadata, which can retain prewarm values for the
connection's lifetime. Malformed identity fields or JSON return `None`. Unrecognized
fields, including conversation content, are skipped in one body scan. Only extracted
fields are allocated; callers retain the original request bytes.

An unknown auxiliary request creates no record. For a known thread, auxiliary
requests update last-seen metadata. Prewarm requests can create records. For forks,
`Observation.parent` and `ThreadRecord.parent` hold the source thread, so `children`
includes both forks and subagents. Resumes preserve resolved lineage.

Eviction removes the oldest record without local open streams. If every slot has
an open stream, admission returns `record: None` and `is_new: false`. A zero limit
retains nothing. The caller must handle this case before routing and must track
in-flight work with `stream_opened` and `stream_closed`, including buffered requests
that should be protected from eviction. Active queries use the supplied idle window;
streaming records remain active regardless of age. Counters saturate on overflow.

Import merges newer records by `last_seen`, keeps local records on equal timestamps,
and retains the newest records within capacity. Imported open-stream counts are
ignored; local counts survive replacement. The caller supplies synchronization and
must reconcile distributed records through the store before import if timestamps
do not establish the desired ordering.

Call `compaction_completed` once per successful completion, before observing the
next Codex window. Window observations take the maximum of the local generation
and the wire window, avoiding double counting in that order.

Run checks with `CARGO_BUILD_JOBS=4`, with a 300-second timeout on each command:

```sh
cargo fmt -p sap-threads
cargo clippy -p sap-threads --all-targets -- -D warnings
cargo test -p sap-threads
```

Five integration tests cover all 36 sanitized fixture lines, extraction edge cases,
fork resolution, active windows, bounded eviction, usage, assignments, generations,
and import/export. Tests perform no I/O beyond reading the compiled-in fixtures.

Contract request: clarify the `sap-core::thread::ThreadRecord.parent` field comment
to include the source of a fork. The current record has no separate `forked_from`
field, and the dashboard needs forks nested under their source. No type change is
required by this implementation.

Known limits: Claude fork resolution uses only retained threads' last response IDs.
Evicted or superseded IDs cannot resolve ancestry. The registry limit counts records,
not bytes. Store/server integration and live client verification belong to later
workstreams. No real providers have been contacted.
