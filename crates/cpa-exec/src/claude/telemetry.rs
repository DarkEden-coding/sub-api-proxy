//! Content-free measurements of the final Messages payload. This module never edits
//! the wire body or sends a token-count request.

use std::collections::VecDeque;

use cpa_core::exec::{CacheBreakpoint, CacheFingerprint, ClaudeCacheTelemetry, ExecRequest};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Ctx, Prepared, detect, identity, session, signals};

fn hash(bytes: &[u8]) -> String {
    session::hex(&Sha256::digest(bytes))
}

fn identifier(domain: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    hash(format!("cpa:claude-cache:{domain}:v1\0{value}").as_bytes())
}

fn snapshot(hasher: &Sha256, bytes: usize, tokens_estimate: usize) -> CacheFingerprint {
    CacheFingerprint {
        hash: session::hex(&hasher.clone().finalize()),
        bytes,
        tokens_estimate,
    }
}

pub(super) fn collect(ctx: &Ctx<'_>, req: &ExecRequest, prepared: &Prepared) -> Option<ClaudeCacheTelemetry> {
    let mut root: Value = serde_json::from_str(&prepared.body).ok()?;
    let encoder = crate::tokenizer::encoder(crate::tokenizer::Encoding::O200kBase).ok()?;
    let string = |key: &str| root[key].as_str().unwrap_or_default().to_owned();
    let mut betas: Vec<String> = prepared
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
        .flat_map(|(_, v)| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        .collect();
    betas.sort();
    betas.dedup();
    let declared = detect::header(&req.headers, "x-cliproxy-request-type");
    let kind = if matches!(declared, "main" | "helper" | "subagent" | "compaction") {
        declared
    } else if req.alt.as_deref() == Some("responses/compact") {
        "compaction"
    } else if signals::subagent(&req.headers, &prepared.body) {
        "subagent"
    } else if signals::probe_or_helper(&prepared.body)
        || detect::detect(&req.headers, &prepared.body, false, &ctx.settings).helper_profile
    {
        "helper"
    } else {
        "main"
    };
    let user = if let Some(user) = root["metadata"]["user_id"].as_str() {
        serde_json::from_str::<Value>(user).unwrap_or(Value::Null)
    } else {
        root["metadata"]["user_id"].clone()
    };
    // The selected credential owns account identity. Caller-supplied metadata on
    // an API-key request must not masquerade as another upstream account.
    let account = identity::account_uuid(ctx.credential);
    let account = if account.is_empty() {
        &ctx.credential.id
    } else {
        &account
    };
    let account_hash = identifier("account", account);
    let session_id = user["session_id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| req.session.clone())
        .unwrap_or_default();
    // Scope includes routing and caller isolation, but stores none of those values.
    let workspace: Vec<_> = prepared
        .headers
        .iter()
        .filter(|(k, _)| {
            ["anthropic-organization-id", "anthropic-project-id", "x-workspace-id"]
                .iter()
                .any(|name| k.eq_ignore_ascii_case(name))
        })
        .collect();
    let scope_hash = hash(
        &serde_json::to_vec(&(
            &account_hash,
            &ctx.base_url,
            &req.caller.principal,
            &user["account_uuid"],
            workspace,
        ))
        .ok()?,
    );
    let retry = detect::header(&req.headers, "x-stainless-retry-count")
        .parse::<usize>()
        .unwrap_or(0)
        > 0
        || declared == "retry";
    let mut telemetry = ClaudeCacheTelemetry {
        schema_version: 1,
        token_estimator: "o200k_base_json_blocks_256b_chunks",
        session_hash: identifier("session", &session_id),
        account_hash,
        scope_hash,
        model: string("model"),
        thinking_type: root["thinking"]["type"].as_str().unwrap_or_default().to_owned(),
        thinking_budget: root["thinking"]["budget_tokens"].as_i64(),
        thinking_hash: hash(&serde_json::to_vec(&root["thinking"]).ok()?),
        effort: root["output_config"]["effort"].as_str().unwrap_or_default().to_owned(),
        betas,
        speed: if prepared.fast { "fast".into() } else { string("speed") },
        service_tier: string("service_tier"),
        request_type: if retry { "retry".into() } else { kind.into() },
        base_request_type: kind.into(),
        ..Default::default()
    };
    let mut prefix = Sha256::new();
    prefix.update(b"cpa:claude-prefix:v1\0");
    // These modes can invalidate reuse even when tools/system/history match.
    for key in [
        "model",
        "thinking",
        "output_config",
        "tool_choice",
        "context_management",
        "speed",
        "service_tier",
    ] {
        prefix.update(key.as_bytes());
        prefix.update(serde_json::to_vec(&root[key]).ok()?);
    }
    prefix.update(serde_json::to_vec(&telemetry.betas).ok()?);
    // Match cloak::valid_cache_control on this serde observation copy. Invalid
    // markers do not count toward preparation's four-marker limit; interpreting
    // them as breakpoints would copy a growing parallel-tool run quadratically.
    let valid_control = |cc: &Value| cc.is_object() && cc["type"] == "ephemeral";
    let automatic = root.get("cache_control").filter(|cc| valid_control(cc)).cloned();
    telemetry.automatic_cache_ttl = automatic.as_ref().map(|c| c["ttl"].as_str().unwrap_or("5m").into());
    let mut automatic_candidate = None;
    let mut prefix_bytes = 0;
    let mut prefix_tokens = 0;
    let mut recent: VecDeque<Vec<CacheFingerprint>> = VecDeque::new();
    let mut previous_group = None;
    for section in ["tools", "system", "messages"] {
        let mut section_hash = Sha256::new();
        let mut section_bytes = 0;
        let mut section_tokens = 0;
        let mut blocks = Vec::new();
        let value = root.get_mut(section);
        if let Some(value) = value {
            if section == "messages" {
                if let Some(messages) = value.as_array_mut() {
                    for (m, message) in messages.iter_mut().enumerate() {
                        let role = message["role"].as_str().unwrap_or_default().to_owned();
                        if let Some(content) = message.get_mut("content") {
                            match content {
                                Value::Array(parts) => {
                                    for (i, part) in parts.iter_mut().enumerate() {
                                        blocks.push((format!("messages.{m}.content.{i}"), role.clone(), part));
                                    }
                                }
                                part => blocks.push((format!("messages.{m}.content"), role, part)),
                            }
                        }
                    }
                }
            } else {
                match value {
                    Value::Array(parts) => {
                        for (i, part) in parts.iter_mut().enumerate() {
                            blocks.push((format!("{section}.{i}"), String::new(), part));
                        }
                    }
                    part => blocks.push((section.into(), String::new(), part)),
                }
            }
        }
        // Select once, before hashing, so a large tool run never copies an
        // ever-growing lookback at every block. Only the final eligible target
        // in a section can supply automatic caching's observation.
        let automatic_index = automatic.as_ref().and_then(|_| {
            blocks.iter().rposition(|(_, _, block)| match block {
                Value::String(text) => !text.is_empty(),
                _ => match block["type"].as_str() {
                    Some("thinking" | "redacted_thinking") => false,
                    Some("text") => block["text"].as_str().is_some_and(|text| !text.is_empty()),
                    _ => true,
                },
            })
        });
        for (index, (path, role, block)) in blocks.into_iter().enumerate() {
            let control = block
                .as_object_mut()
                .and_then(|o| o.remove("cache_control"))
                .filter(valid_control);
            let json = serde_json::to_string(&block).ok()?;
            // ponytail: O200k is the existing local Claude count estimator, not
            // Anthropic's tokenizer. Counts exclude media tokenization and framing;
            // replay calibrates cumulative positions to returned read+write usage.
            // Bound regex recursion and BPE work on long repeated strings: an
            // unbounded encode_ordinary can panic with regex StackOverflow. Chunk
            // seams affect only this labelled estimate, never the upstream payload.
            let mut text = json.as_str();
            let mut tokens = 0;
            while !text.is_empty() {
                let mut end = text.len().min(256);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                tokens += encoder.encode_ordinary(&text[..end]).len();
                text = &text[end..];
            }
            let bytes = serde_json::to_vec(&(&path, &role, &*block)).ok()?;
            // Length framing prevents concatenation ambiguities. Only cache_control
            // is excluded: every final text, tool name, signature and role participates.
            for hasher in [&mut prefix, &mut section_hash] {
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            prefix_bytes += bytes.len();
            section_bytes += bytes.len();
            prefix_tokens += tokens;
            section_tokens += tokens;
            let current = snapshot(&prefix, prefix_bytes, prefix_tokens);
            // Anthropic checks 20 positions including the breakpoint. A parallel
            // run shares one position; keep its boundaries for explicit mid-run writes.
            let group = block["type"]
                .as_str()
                .filter(|kind| matches!(*kind, "tool_use" | "tool_result"))
                .map(|kind| {
                    (
                        path.split(".content").next().unwrap_or(&path).to_owned(),
                        kind.to_owned(),
                    )
                });
            if group.is_none() || group != previous_group {
                recent.push_back(Vec::new());
                if recent.len() > 20 {
                    recent.pop_front();
                }
            }
            previous_group = group;
            recent.back_mut()?.push(current.clone());
            let lookback = if control.is_some() || automatic_index == Some(index) {
                let mut positions: Vec<_> = recent.iter().flatten().cloned().collect();
                positions.pop(); // The current boundary is already in `prefix`.
                positions
            } else {
                Vec::new()
            };
            if let Some(control) = &control {
                telemetry.breakpoints.push(CacheBreakpoint {
                    path: path.clone(),
                    ttl: control["ttl"].as_str().unwrap_or("5m").into(),
                    prefix: current.clone(),
                    lookback: lookback.clone(),
                });
                if section == "messages" {
                    telemetry.history_prefix = snapshot(&section_hash, section_bytes, section_tokens);
                }
            }
            if let Some(automatic) = &automatic
                && automatic_index == Some(index)
            {
                // At the last eligible block an explicit marker makes automatic
                // caching a no-op (or a provider error if TTLs conflict). Retain
                // the top-level TTL separately, never invent a duplicate write.
                automatic_candidate = control.is_none().then(|| {
                    (
                        CacheBreakpoint {
                            path: format!("cache_control:{path}"),
                            ttl: automatic["ttl"].as_str().unwrap_or("5m").into(),
                            prefix: current,
                            lookback,
                        },
                        if section == "messages" {
                            snapshot(&section_hash, section_bytes, section_tokens)
                        } else {
                            CacheFingerprint::default()
                        },
                    )
                });
            }
        }
        match section {
            "tools" => telemetry.tools = snapshot(&section_hash, section_bytes, section_tokens),
            "system" => telemetry.system = snapshot(&section_hash, section_bytes, section_tokens),
            _ => {}
        }
    }
    if let Some((point, history)) = automatic_candidate {
        telemetry.breakpoints.push(point);
        telemetry.history_prefix = history;
    }
    Some(telemetry)
}
