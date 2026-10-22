// Mirrors crates/sap-core/src/api.rs and the enums it references. Keys are snake_case and
// times are RFC 3339 strings, exactly as the server serializes them.

export type Provider = "claude" | "codex";
export type NodeId = string;
export type AccountId = string;
/** `"<provider>:<id>"`, for example `claude:7fe0…` or `claude:<session>/<agent>`. */
export type ThreadKey = `${Provider}:${string}`;
export type Timestamp = string;

export type WindowKind = "five_hour" | "weekly" | { other: string };
export type ObservationSource = "passive" | "probe";
export type CapacitySource = "default" | "plan" | "learned" | "override";
export type ClientKind = "claude_code" | "codex_cli" | "other";
export type LineageKind = "root" | "subagent" | "fork";
export type NodeRole = "primary" | "fallback";
export type NodeState = "serving" | "starting" | "degraded" | "draining";
export type AccountState = "active" | "idle" | "cooling" | "exhausted" | "disabled" | "login_needed";
export type EventKind =
  | "thread_assigned"
  | "thread_moved"
  | "compaction"
  | "failover"
  | "failback"
  | "sync_conflict"
  | "account_state"
  | "login_change";

export interface FlowSnapshot {
  now: Timestamp;
  /** The node that produced this snapshot. */
  node: NodeId;
  nodes: NodeView[];
  accounts: AccountView[];
  /** Only active threads. */
  threads: ThreadView[];
  /** Most recent first. */
  events: EventView[];
  stats: StatsView;
  open_conflicts: number;
}

export interface AccountView {
  id: AccountId;
  provider: Provider;
  label: string;
  email: string | null;
  plan: string | null;
  capacity_multiplier: number;
  capacity_source: CapacitySource;
  priority: number;
  state: AccountState;
  /** Share of new threads, 0 to 100 per provider. */
  weight_percent: number;
  windows: WindowView[];
  active_threads: number;
  /** When an exhausted or cooling account becomes usable, if known. */
  available_at: Timestamp | null;
  login_needed_on: NodeId[];
  /** Quota data is missing or stale. */
  stale: boolean;
}

export interface WindowView {
  kind: WindowKind;
  used_percent: number;
  reset_at: Timestamp | null;
  observed_at: Timestamp;
  source: ObservationSource;
}

export interface ThreadView {
  key: ThreadKey;
  client: ClientKind;
  /** Client machine, from the local router. */
  machine: string | null;
  title: string | null;
  lineage: LineageKind;
  parent: ThreadKey | null;
  /** Claude agent type or Codex agent path. */
  agent_label: string | null;
  account: AccountId | null;
  /** Human label from `routing::Reason::label`. */
  reason: string | null;
  serving_node: NodeId | null;
  streaming: boolean;
  cache_hit_percent: number | null;
  compactions: number;
  requests: number;
  first_seen: Timestamp;
  last_seen: Timestamp;
}

export interface NodeView {
  id: NodeId;
  role: NodeRole;
  state: NodeState;
  /** Reachable from the node producing the snapshot. */
  reachable: boolean;
  version: string | null;
  requests_per_minute: number;
  open_streams: number;
  added_latency_p50_ms: number | null;
  rss_bytes: number | null;
  sync_lag_secs: number | null;
  last_failover_at: Timestamp | null;
  last_seen: Timestamp | null;
}

export interface EventView {
  id: string;
  at: Timestamp;
  kind: EventKind;
  message: string;
  node: NodeId;
  thread: ThreadKey | null;
  account: AccountId | null;
}

export interface StatsView {
  requests_3h: number;
  cache_hit_percent_3h: number | null;
  added_latency_p50_ms: number | null;
  rss_bytes: number | null;
  /** Fifteen-minute buckets over the last three hours, oldest first. */
  series: SeriesPoint[];
}

export interface SeriesPoint {
  start: Timestamp;
  requests: number;
  cache_read_tokens: number;
  input_tokens: number;
}

/** Fields to change; absent keys stay as they are. */
export interface AccountPatch {
  label?: string;
  disabled?: boolean;
  priority?: number;
  /** A number sets the override, `null` clears it, an absent key leaves it. */
  capacity_override?: number | null;
}

export interface LoginRequest {
  provider: Provider;
  /** Re-login an existing account on this node; null adds a new account. */
  account: AccountId | null;
}

export interface LoginStart {
  url: string;
  login_id: string;
}

export interface AccountHistory {
  account: AccountId;
  samples: HistoryPoint[];
}

export interface HistoryPoint {
  t: Timestamp;
  five_hour_used_percent: number | null;
  weekly_used_percent: number | null;
}

export interface AssignRequest {
  account: AccountId;
}

export interface ResolveRequest {
  /** Keep the value from this node. */
  keep: NodeId;
}

export interface ApiError {
  error: string;
  message: string;
}

// sap-core/src/sync.rs and hlc.rs, as served by /sap/api/conflicts.

export type SyncTable =
  | "accounts"
  | "settings"
  | "client_access"
  | "threads"
  | "quota_observations"
  | "usage_buckets"
  | "events"
  | "conflicts";

export interface Hlc {
  wall_ms: number;
  counter: number;
  node: NodeId;
}

export interface ConflictSide {
  node: NodeId;
  hlc: Hlc;
  value: unknown;
}

export interface Conflict {
  id: string;
  table: SyncTable;
  key: string;
  field: string;
  /** The value currently applied (the higher HLC) and the one it replaced. */
  applied: ConflictSide;
  other: ConflictSide;
  detected_at: Timestamp;
  resolved: boolean;
}
