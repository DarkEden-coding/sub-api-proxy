use crate::{
    hooks::{RequestState, reply},
    node::Node,
};
use cpa_server::sap_hooks::{SapCandidate, SapPick, SapRequest};
use sap_core::{
    Utc,
    ids::Provider,
    routing::{Decision, RoutingEvent, RoutingPolicy},
    thread::{Assignment, Continuation},
};
use std::collections::BTreeSet;

impl Node {
    pub(crate) fn choose(&self, request: &SapRequest, candidates: &[SapCandidate]) -> anyhow::Result<SapPick> {
        anyhow::ensure!(
            !request
                .data
                .downcast_ref::<RequestState>()
                .is_some_and(|r| r.persistence_failed),
            "thread persistence failed"
        );
        let mapping = self.discover()?;
        let available: BTreeSet<_> = candidates
            .iter()
            .filter_map(|c| mapping.get(&c.credential).cloned())
            .collect();
        let mut state = self.lock();
        let key = &request.identity.key;
        let Some(record) = state.registry.get(key).cloned() else {
            return Ok(reply(key.provider, 429, "Thread registry is full", None));
        };
        let event = if let Some(current) = record.assignment.clone() {
            if let Some(local) = state.accounts.get(&current.account)
                && let Some(reset_at) = local.exhausted.filter(|reset| reset.is_none_or(|t| t > Utc::now()))
            {
                RoutingEvent::AfterOutcome {
                    current,
                    outcome: sap_core::routing::Outcome::Exhausted {
                        window: local
                            .exhausted_window
                            .clone()
                            .unwrap_or(sap_core::quota::WindowKind::Other("unknown".into())),
                        reset_at,
                    },
                    first_failure_at: Utc::now(),
                }
            } else if let Some((outcome, first_failure_at)) = state.failures.get(key) {
                RoutingEvent::AfterOutcome {
                    current,
                    outcome: outcome.clone(),
                    first_failure_at: *first_failure_at,
                }
            } else if state.compacted.contains(key) {
                RoutingEvent::CompactionCompleted { current }
            } else {
                RoutingEvent::Continue { current }
            }
        } else if let Some(parent) = record.parent.as_ref() {
            RoutingEvent::ChildStart {
                parent_account: state
                    .registry
                    .get(parent)
                    .and_then(|p| p.assignment.as_ref())
                    .map(|a| a.account.clone()),
            }
        } else {
            RoutingEvent::NewThread
        };
        let input = self.input(&state, key.provider, request.identity.model.clone(), event, &available)?;
        let decision = sap_routing::ThreadAffinityPolicy::new(self.config.routing.clone()).decide(&input);
        let account = match decision {
            Decision::Use { account, reason } => {
                state.compacted.remove(key);
                let moved = record.assignment.as_ref().is_some_and(|a| a.account != account)
                    || record.serving_node.as_ref().is_some_and(|n| n != &self.config.node);
                if moved {
                    state.resend.insert(key.clone());
                    state.registry.record_response(key, "");
                }
                state.registry.assign(
                    key,
                    Assignment {
                        account: account.clone(),
                        reason: reason.clone(),
                        at: Utc::now(),
                    },
                );
                self.persist(&mut state, key, true)?;
                if record.assignment.as_ref().is_none_or(|a| a.account != account) {
                    self.event(
                        if moved {
                            sap_core::api::EventKind::ThreadMoved
                        } else {
                            sap_core::api::EventKind::ThreadAssigned
                        },
                        Some(key.clone()),
                        Some(account.clone()),
                        &reason.label(),
                    );
                    state.failures.remove(key);
                }
                account
            }
            Decision::RetrySame { account, at } => {
                // ponytail: synchronous select cannot delay dispatch; return retry_at until the hook supports an async retry answer.
                if at > Utc::now() {
                    return Ok(reply(
                        key.provider,
                        429,
                        "Retry the current account after cooldown",
                        Some(at),
                    ));
                }
                account
            }
            Decision::Reject {
                earliest_usable_at,
                message,
                ..
            } => return Ok(reply(key.provider, 429, &message, earliest_usable_at)),
        };
        if matches!(request.identity.continuation, Continuation::Continue { .. })
            && (state.resend.contains(key)
                || matches!(&request.identity.continuation, Continuation::Continue {previous_id} if record.last_response_id.as_ref() != Some(previous_id))
                || record.assignment.is_none())
        {
            if key.provider == Provider::Codex
                && request.data.downcast_ref::<RequestState>().is_some_and(|r| r.websocket)
            {
                return Ok(SapPick::CloseSocket);
            }
            return Ok(reply(
                key.provider,
                400,
                "Previous response is unavailable on this account; resend the full history",
                None,
            ));
        }
        if matches!(request.identity.continuation, Continuation::FullHistory) {
            state.resend.remove(key);
        }
        Ok(candidates
            .iter()
            .find(|c| mapping.get(&c.credential) == Some(&account))
            .map(|c| SapPick::Credential(c.credential.clone()))
            .unwrap_or_else(|| reply(key.provider, 429, "Account login unavailable", None)))
    }
}
