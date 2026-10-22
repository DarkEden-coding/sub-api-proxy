use std::collections::BTreeMap;
use std::time::Duration;

use sap_core::ids::{NodeId, Provider, ThreadKey};
use sap_core::thread::{Assignment, LineageKind, RequestClass, ThreadIdentity, ThreadRecord, ThreadUsage};
use sap_core::{DateTime, Utc};

/// Result of observing a request, including resolved fork ancestry.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Absent for unknown auxiliary requests, or when all slots are streaming.
    pub record: Option<ThreadRecord>,
    /// True only when a record was inserted.
    pub is_new: bool,
    pub lineage: LineageKind,
    /// Source thread for forks, parent thread for subagents.
    pub parent: Option<ThreadKey>,
}

/// In-memory records. Callers provide synchronization and timestamps.
///
/// Eviction considers records with no local open streams inactive, oldest first.
/// When every slot is streaming, a new thread is not retained. A zero limit disables
/// retention. Returned lists are ordered by key for deterministic export and display.
#[derive(Debug)]
pub struct Registry {
    max_records: usize,
    records: BTreeMap<ThreadKey, ThreadRecord>,
}

impl Registry {
    pub fn new(max_records: usize) -> Self {
        Self {
            max_records,
            records: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, identity: &ThreadIdentity, now: DateTime<Utc>, node: NodeId) -> Observation {
        let mut lineage = identity.lineage;
        let mut parent = identity.forked_from.clone().or_else(|| identity.parent.clone());
        if !self.records.contains_key(&identity.key)
            && identity.key.provider == Provider::Claude
            && lineage == LineageKind::Root
            && let Some(previous) = &identity.previous_message_id
            && let Some(source) = self
                .records
                .values()
                .find(|r| r.key.provider == Provider::Claude && r.last_response_id.as_ref() == Some(previous))
        {
            lineage = LineageKind::Fork;
            parent = Some(source.key.clone());
        }
        let mut is_new = false;
        if !self.records.contains_key(&identity.key) && identity.class != RequestClass::Auxiliary && self.make_room() {
            self.records.insert(
                identity.key.clone(),
                ThreadRecord {
                    key: identity.key.clone(),
                    client: identity.client,
                    lineage,
                    root_session: identity.root_session.clone(),
                    parent: parent.clone(),
                    client_host: identity.client_host.clone(),
                    agent_label: identity.agent_label.clone(),
                    title: None,
                    assignment: None,
                    compaction_generation: identity.window_number.unwrap_or(0),
                    last_response_id: None,
                    first_seen: now,
                    last_seen: now,
                    serving_node: Some(node.clone()),
                    open_streams: 0,
                    usage: ThreadUsage::default(),
                },
            );
            is_new = true;
        }
        let record = self.records.get_mut(&identity.key).map(|record| {
            record.last_seen = record.last_seen.max(now);
            record.serving_node = Some(node);
            if identity.client_host.is_some() {
                record.client_host.clone_from(&identity.client_host);
            }
            if identity.agent_label.is_some() {
                record.agent_label.clone_from(&identity.agent_label);
            }
            if let Some(window) = identity.window_number {
                record.compaction_generation = record.compaction_generation.max(window);
            }
            // Once resolved, lineage survives resumes and requests without ancestry metadata.
            lineage = record.lineage;
            parent.clone_from(&record.parent);
            record.clone()
        });
        Observation {
            record,
            is_new,
            lineage,
            parent,
        }
    }

    fn make_room(&mut self) -> bool {
        if self.max_records == 0 {
            return false;
        }
        if self.records.len() < self.max_records {
            return true;
        }
        let victim = self
            .records
            .values()
            .filter(|r| r.open_streams == 0)
            .min_by_key(|r| (r.last_seen, &r.key))
            .map(|r| r.key.clone());
        if let Some(key) = victim {
            self.records.remove(&key);
            true
        } else {
            false
        }
    }

    pub fn get(&self, key: &ThreadKey) -> Option<&ThreadRecord> {
        self.records.get(key)
    }

    pub fn record_response(&mut self, key: &ThreadKey, response_id: impl Into<String>) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        record.last_response_id = Some(response_id.into());
        true
    }

    pub fn record_usage(&mut self, key: &ThreadKey, delta: ThreadUsage) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        let usage = &mut record.usage;
        usage.requests = usage.requests.saturating_add(delta.requests);
        usage.input_tokens = usage.input_tokens.saturating_add(delta.input_tokens);
        usage.cache_read_tokens = usage.cache_read_tokens.saturating_add(delta.cache_read_tokens);
        usage.cache_write_tokens = usage.cache_write_tokens.saturating_add(delta.cache_write_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(delta.output_tokens);
        true
    }

    pub fn stream_opened(&mut self, key: &ThreadKey) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        record.open_streams = record.open_streams.saturating_add(1);
        true
    }

    pub fn stream_closed(&mut self, key: &ThreadKey) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        record.open_streams = record.open_streams.saturating_sub(1);
        true
    }

    /// Call on successful completion, before observing the next Codex window.
    /// Observation takes the maximum of this count and the wire window number.
    pub fn compaction_completed(&mut self, key: &ThreadKey) -> Option<u32> {
        let record = self.records.get_mut(key)?;
        record.compaction_generation = record.compaction_generation.saturating_add(1);
        Some(record.compaction_generation)
    }

    pub fn assign(&mut self, key: &ThreadKey, assignment: Assignment) -> bool {
        let Some(record) = self.records.get_mut(key) else {
            return false;
        };
        record.assignment = Some(assignment);
        true
    }

    pub fn active(&self, now: DateTime<Utc>, idle_window: Duration) -> Vec<ThreadRecord> {
        self.records
            .values()
            .filter(|r| {
                r.open_streams > 0
                    || now
                        .signed_duration_since(r.last_seen)
                        .to_std()
                        .map_or(true, |age| age <= idle_window)
            })
            .cloned()
            .collect()
    }

    pub fn children(&self, key: &ThreadKey) -> Vec<ThreadRecord> {
        self.records
            .values()
            .filter(|r| r.parent.as_ref() == Some(key))
            .cloned()
            .collect()
    }

    pub fn export(&self) -> Vec<ThreadRecord> {
        self.records.values().cloned().collect()
    }

    /// Merges newer records, preserving local stream counts. Imported stream counts
    /// are always discarded because sockets are local state. Newest records win the
    /// capacity budget; existing streaming records cannot be evicted or overwritten
    /// by older records. Equal timestamps retain the local record.
    pub fn import(&mut self, mut records: Vec<ThreadRecord>) {
        records.sort_by(|a, b| (a.last_seen, &a.key).cmp(&(b.last_seen, &b.key)));
        for mut record in records {
            if let Some(local) = self.records.get(&record.key) {
                if local.last_seen >= record.last_seen {
                    continue;
                }
                record.open_streams = local.open_streams;
            } else {
                record.open_streams = 0;
                if self.records.len() >= self.max_records
                    && self
                        .records
                        .values()
                        .filter(|r| r.open_streams == 0)
                        .all(|r| r.last_seen > record.last_seen)
                {
                    continue;
                }
                if !self.make_room() {
                    continue;
                }
            }
            self.records.insert(record.key.clone(), record);
        }
    }
}
