use http::HeaderMap;
use sap_core::health::CLIENT_HOST_HEADER;
use sap_core::ids::{Provider, ThreadKey};
use sap_core::thread::{ClientKind, CompactionTrigger, Continuation, LineageKind, RequestClass, ThreadIdentity};
use serde::Deserialize;

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().filter(|s| !s.is_empty())
}

fn trigger(value: Option<&str>) -> Option<CompactionTrigger> {
    match value {
        Some("manual") => Some(CompactionTrigger::Manual),
        Some("auto") => Some(CompactionTrigger::Auto),
        _ => None,
    }
}

#[derive(Default, Deserialize)]
struct ClaudeBody {
    model: Option<String>,
    metadata: Option<ClaudeMetadata>,
    thread: Option<ClaudeThread>,
    diagnostics: Option<Diagnostics>,
}
#[derive(Deserialize)]
struct ClaudeMetadata {
    user_id: Option<UserId>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum UserId {
    Encoded(String),
    Object(Session),
}
#[derive(Deserialize)]
struct Session {
    session_id: Option<String>,
}
#[derive(Deserialize)]
struct ClaudeThread {
    r#type: String,
    previous_message_id: Option<String>,
}
#[derive(Deserialize)]
struct Diagnostics {
    previous_message_id: Option<String>,
}

/// Identifies a Claude messages request without modifying or retaining its body.
/// Malformed JSON or a continuation without its required ID returns `None`.
pub fn identify_claude(headers: &HeaderMap, body: &[u8]) -> Option<ThreadIdentity> {
    // Serde ignores all conversation fields, scanning their bytes once without allocating them.
    let body: ClaudeBody = serde_json::from_slice(body).ok()?;
    let session = header(headers, "x-claude-code-session-id")
        .map(str::to_owned)
        .or_else(|| {
            let user = body.metadata?.user_id?;
            match user {
                UserId::Encoded(encoded) => serde_json::from_str::<Session>(&encoded).ok()?.session_id,
                UserId::Object(session) => session.session_id,
            }
        })
        .filter(|s| !s.is_empty())?;
    let agent = header(headers, "x-claude-code-agent-id");
    let class = match header(headers, "x-claude-code-request-class") {
        Some("auxiliary") => RequestClass::Auxiliary,
        Some("compaction") => RequestClass::Compaction,
        Some("subagent") => RequestClass::SubagentTurn,
        _ if agent.is_some() => RequestClass::SubagentTurn,
        _ => RequestClass::Turn,
    };
    let continuation = match body.thread {
        Some(thread) if thread.r#type == "continue" => Continuation::Continue {
            previous_id: thread.previous_message_id.filter(|s| !s.is_empty())?,
        },
        _ => Continuation::FullHistory,
    };
    let marked = header(headers, "x-claude-code-session-id").is_some()
        || header(headers, "x-claude-code-request-class").is_some()
        || header(headers, "user-agent").is_some_and(|s| s.starts_with("claude-cli/"));
    Some(ThreadIdentity {
        key: ThreadKey::new(
            Provider::Claude,
            agent.map_or_else(|| session.clone(), |a| format!("{session}/{a}")),
        ),
        client: if marked {
            ClientKind::ClaudeCode
        } else {
            ClientKind::Other
        },
        class,
        lineage: if agent.is_some() {
            LineageKind::Subagent
        } else {
            LineageKind::Root
        },
        parent: agent.map(|_| ThreadKey::new(Provider::Claude, session.clone())),
        root_session: session,
        forked_from: None,
        previous_message_id: body.diagnostics.and_then(|d| d.previous_message_id),
        continuation,
        compaction: (class == RequestClass::Compaction)
            .then(|| trigger(header(headers, "x-claude-code-compaction")))
            .flatten(),
        window_number: None,
        agent_label: header(headers, "x-claude-code-agent-type").map(str::to_owned),
        turn_id: header(headers, "x-claude-code-prompt-id").map(str::to_owned),
        model: body.model,
        client_host: header(headers, CLIENT_HOST_HEADER).map(str::to_owned),
    })
}

#[derive(Deserialize)]
struct CodexBody {
    model: Option<String>,
    previous_response_id: Option<String>,
    generate: Option<bool>,
    client_metadata: Option<ClientMetadata>,
}
#[derive(Deserialize)]
struct ClientMetadata {
    #[serde(rename = "x-codex-turn-metadata")]
    turn_metadata: Option<String>,
    #[serde(rename = "x-codex-window-id")]
    window_id: Option<String>,
}
#[derive(Default, Deserialize)]
struct TurnMetadata {
    parent_thread_id: Option<String>,
    forked_from_thread_id: Option<String>,
    thread_source: Option<String>,
    request_kind: Option<String>,
    agent_name: Option<String>,
    turn_id: Option<String>,
    window_number: Option<u32>,
    window_id: Option<String>,
    compaction: Option<Compaction>,
}
#[derive(Deserialize)]
struct Compaction {
    trigger: Option<String>,
}

/// Identifies an HTTP Responses body or a WebSocket `response.create` frame.
/// Frame metadata overrides connection-scoped upgrade metadata.
pub fn identify_codex(headers: &HeaderMap, body: &[u8]) -> Option<ThreadIdentity> {
    let body: CodexBody = serde_json::from_slice(body).ok()?;
    let id = header(headers, "thread-id")?;
    let frame = body.client_metadata.as_ref();
    let metadata = frame
        .and_then(|m| m.turn_metadata.as_deref())
        .or_else(|| header(headers, "x-codex-turn-metadata"));
    let metadata: TurnMetadata = match metadata {
        Some(raw) => serde_json::from_str(raw).ok()?,
        None => TurnMetadata::default(),
    };
    let parent = metadata.parent_thread_id.filter(|s| !s.is_empty());
    let fork = metadata.forked_from_thread_id.filter(|s| !s.is_empty());
    let lineage = if fork.is_some() {
        LineageKind::Fork
    } else if parent.is_some() && metadata.thread_source.as_deref() == Some("subagent") {
        LineageKind::Subagent
    } else {
        LineageKind::Root
    };
    let class = match metadata.request_kind.as_deref() {
        Some("compaction") => RequestClass::Compaction,
        Some("prewarm") => RequestClass::Prewarm,
        _ if body.generate == Some(false) => RequestClass::Prewarm,
        _ if lineage == LineageKind::Subagent => RequestClass::SubagentTurn,
        _ => RequestClass::Turn,
    };
    let window_number = frame
        .and_then(|m| m.window_id.as_deref())
        .or(metadata.window_id.as_deref())
        .and_then(|s| s.rsplit_once(':')?.1.parse().ok())
        .or(metadata.window_number)
        .or_else(|| header(headers, "x-codex-window-id")?.rsplit_once(':')?.1.parse().ok());
    Some(ThreadIdentity {
        key: ThreadKey::new(Provider::Codex, id),
        client: ClientKind::CodexCli,
        class,
        lineage,
        root_session: header(headers, "session-id").unwrap_or(id).to_owned(),
        parent: (lineage == LineageKind::Subagent)
            .then(|| parent.map(|p| ThreadKey::new(Provider::Codex, p)))
            .flatten(),
        forked_from: fork.map(|p| ThreadKey::new(Provider::Codex, p)),
        previous_message_id: None,
        continuation: body
            .previous_response_id
            .map_or(Continuation::FullHistory, |previous_id| Continuation::Continue {
                previous_id,
            }),
        compaction: if class == RequestClass::Compaction {
            trigger(metadata.compaction.as_ref().and_then(|c| c.trigger.as_deref()))
        } else {
            None
        },
        window_number,
        agent_label: metadata.agent_name,
        turn_id: metadata.turn_id.filter(|s| !s.is_empty()),
        model: body.model,
        client_host: header(headers, CLIENT_HOST_HEADER).map(str::to_owned),
    })
}

/// Dispatches supported inference paths. Pass the URL path without a query string.
pub fn identify(path: &str, headers: &HeaderMap, body: &[u8]) -> Option<ThreadIdentity> {
    match path {
        "/v1/messages" => identify_claude(headers, body),
        "/v1/responses" | "/responses" | "/backend-api/codex/responses" => identify_codex(headers, body),
        _ => None,
    }
}
