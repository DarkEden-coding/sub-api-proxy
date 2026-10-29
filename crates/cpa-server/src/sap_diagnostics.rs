//! Content-free diagnostics for SAP selection and execution failures. Raw errors
//! belong to request_logging's protected capture sink, never the process log.

use cpa_core::exec::ExecError;
use sap_core::thread::ThreadIdentity;
use sha2::{Digest, Sha256};

use crate::runtime::AcquireError;

fn fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..16].to_owned()
}

// Only emit constants: providers may echo credentials or prompt content even in
// error.code/type. Unknown fields and free-form messages stay in the capture sink.
fn error_label(body: &[u8], field: &str) -> &'static str {
    const LABELS: &[&str] = &[
        "usage_limit_reached",
        "rate_limit_exceeded",
        "rate_limit_error",
        "insufficient_quota",
        "quota_exceeded",
        "model_at_capacity",
        "model_not_found",
        "model_not_supported",
        "model_access_denied",
        "authentication_error",
        "invalid_api_key",
        "auth_unavailable",
        "invalid_request_error",
        "context_length_exceeded",
        "previous_response_not_found",
        "overloaded_error",
        "server_error",
    ];
    let Ok(body) = std::str::from_utf8(body) else {
        return "unrecognized";
    };
    for prefix in ["error", "response.error", "body.error", ""] {
        let path = if prefix.is_empty() {
            field.to_owned()
        } else {
            format!("{prefix}.{field}")
        };
        let value = gjson::get(body, &path);
        if let Some(label) = LABELS.iter().find(|label| **label == value.str()) {
            return label;
        }
    }
    "unrecognized"
}

fn routing_reason(body: &[u8]) -> &'static str {
    let Ok(body) = std::str::from_utf8(body) else {
        return "unrecognized";
    };
    match gjson::get(body, "error.message").str() {
        "No eligible account can serve this request" => "no_eligible_account",
        "Retry the current account after cooldown" => "cooldown",
        "Temporary limit exceeded the retry deadline; keep the thread assignment" => "retry_deadline",
        "Thread registry is full" => "registry_full",
        "Routing state unavailable" => "routing_state_unavailable",
        "Account login unavailable" => "login_unavailable",
        "Previous response is unavailable on this account; resend the full history" => "history_required",
        _ => "unrecognized",
    }
}

pub(crate) fn selection(identity: &ThreadIdentity, request_id: &str, error: &AcquireError) {
    let (kind, wait) = match error {
        AcquireError::Unavailable { retry_after, .. } => ("unavailable", *retry_after),
        AcquireError::Cooldown { wait, .. } => ("cooldown", Some(*wait)),
        AcquireError::Prepare { .. } => ("prepare", None),
        AcquireError::Rejected { .. } => ("scheduler_rejected", None),
        AcquireError::Scheduler(_) => ("scheduler_error", None),
    };
    tracing::warn!(
        request_id, stage = "candidate_selection", kind,
        provider = %identity.key.provider,
        model_hash = %fingerprint(identity.model.as_deref().unwrap_or_default()),
        thread_hash = %fingerprint(&identity.key.id),
        retry_after_seconds = ?wait.map(|d| d.as_secs()),
        "SAP candidate selection failed before routing hook"
    );
    if let AcquireError::Prepare { error, .. } | AcquireError::Scheduler(error) = error {
        failure(identity, request_id, "candidate_selection", Some(0), error);
    }
}

pub(crate) fn failure(
    identity: &ThreadIdentity,
    request_id: &str,
    stage: &'static str,
    candidates: Option<usize>,
    error: &ExecError,
) {
    tracing::warn!(
        request_id, stage,
        provider = %identity.key.provider,
        model_hash = %fingerprint(identity.model.as_deref().unwrap_or_default()),
        thread_hash = %fingerprint(&identity.key.id),
        status = error.status, scope = ?error.scope, candidate_count = ?candidates,
        error_type = error_label(&error.body, "type"),
        error_code = error_label(&error.body, "code"),
        routing_reason = if stage == "routing_hook" { routing_reason(&error.body) } else { "not_applicable" },
        retry_after_seconds = ?error.retry_after.map(|d| d.as_secs()),
        "SAP request failure"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_labels_are_allowlisted_including_nested_terminal_errors() {
        let body = br#"{"type":"response.failed","response":{"error":{"type":"usage_limit_reached","code":"secret-key","message":"Bearer secret-key"}}}"#;
        assert_eq!(error_label(body, "type"), "usage_limit_reached");
        assert_eq!(error_label(body, "code"), "unrecognized");
        assert_eq!(error_label(b"Bearer secret-key", "type"), "unrecognized");
        assert_eq!(error_label(&[0xff], "type"), "unrecognized");
    }
}
