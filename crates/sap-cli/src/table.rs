use sap_core::api::FlowSnapshot;
use serde::Serialize;
use serde_json::Value;

fn cell(value: &Value) -> String {
    let text = match value {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Server labels must not inject terminal control sequences or extra rows.
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub fn render<T: Serialize>(data: &T, columns: &[(&str, &str)]) -> String {
    let value = serde_json::to_value(data).expect("API values serialize");
    let rows = match value {
        Value::Array(rows) => rows,
        row => vec![row],
    };
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| columns.iter().map(|(_, key)| cell(&row[*key])).collect())
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
                .max(name.len())
        })
        .collect();
    let format_row = |row: Vec<String>| {
        row.iter()
            .enumerate()
            .map(|(i, s)| format!("{s}{}", " ".repeat(widths[i] - s.chars().count())))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut output = format_row(columns.iter().map(|(name, _)| (*name).into()).collect());
    output.push('\n');
    for row in cells {
        output.push_str(&format_row(row));
        output.push('\n');
    }
    if rows.is_empty() {
        output.push_str("(none)\n");
    }
    output
}

pub const NODES: &[(&str, &str)] = &[
    ("NODE", "id"),
    ("ROLE", "role"),
    ("STATE", "state"),
    ("REACHABLE", "reachable"),
    ("STREAMS", "open_streams"),
    ("SYNC LAG s", "sync_lag_secs"),
];
pub const ACCOUNTS: &[(&str, &str)] = &[
    ("ACCOUNT", "id"),
    ("PROVIDER", "provider"),
    ("LABEL", "label"),
    ("STATE", "state"),
    ("PRIORITY", "priority"),
    ("WEIGHT %", "weight_percent"),
    ("THREADS", "active_threads"),
];
pub const THREADS: &[(&str, &str)] = &[
    ("THREAD", "key"),
    ("TITLE", "title"),
    ("MACHINE", "machine"),
    ("ACCOUNT", "account"),
    ("NODE", "serving_node"),
    ("REASON", "reason"),
];
pub const EVENTS: &[(&str, &str)] = &[
    ("TIME", "at"),
    ("KIND", "kind"),
    ("NODE", "node"),
    ("MESSAGE", "message"),
];
pub const CONFLICTS: &[(&str, &str)] = &[
    ("ID", "id"),
    ("RECORD", "key"),
    ("FIELD", "field"),
    ("APPLIED", "applied"),
    ("OTHER", "other"),
    ("RESOLVED", "resolved"),
];

pub fn flow(snapshot: &FlowSnapshot) -> String {
    format!(
        "{}\n{} accounts, {} active threads, {} open conflicts\nRequests in 3h: {}   Cache hit: {}%\n",
        render(&snapshot.nodes, NODES),
        snapshot.accounts.len(),
        snapshot.threads.len(),
        snapshot.open_conflicts,
        snapshot.stats.requests_3h,
        snapshot
            .stats
            .cache_hit_percent_3h
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}
