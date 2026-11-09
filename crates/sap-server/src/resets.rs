//! Manual usage resets: reading what each account can spend (during a quota refresh) and
//! spending one (`POST /sap/api/accounts/{id}/reset`). The provider calls live in
//! `cpa_exec::reset_credits`.

use crate::{
    control::{BoxFuture, ControlError, ControlResult},
    node::Node,
};
use cpa_core::{config::Config, credential::Credential};
use cpa_exec::{
    reset_credits::{Blocker, RedeemError, Redeemed, ResetCredits, new_request_id},
    usage_probe::ProbeError,
};
use cpa_server::Runtime;
use sap_core::{
    DateTime, Utc,
    api::{EventKind, ResetCreditView, ResetOutcome, ResetResult, ResetsView},
    ids::AccountId,
    routing::Outcome,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

/// How long an unanswered spend may be retried with its request ID.
const RETRY_WINDOW: Duration = Duration::from_secs(600);

/// A spend sent without a terminal answer. Retrying with the same request ID (and Claude
/// grant) cannot spend twice.
struct Unanswered {
    request_id: String,
    grant: Option<String>,
    at: Instant,
}

// ponytail: process-local, like poll metadata. Peers show only their own reads, and an
// unanswered spend is forgotten on restart or after RETRY_WINDOW; persist it if double
// spends after a crash mid-request become a concern.
#[derive(Default)]
pub(crate) struct Resets {
    views: BTreeMap<AccountId, ResetsView>,
    spending: BTreeSet<AccountId>,
    unanswered: BTreeMap<AccountId, Unanswered>,
}

fn blocked_text(blocker: &Blocker) -> String {
    match blocker {
        Blocker::Ineligible => "The provider does not offer resets to this account".into(),
        Blocker::NoneLeft => "No resets left".into(),
        Blocker::NotLimited => "Usable once a usage limit is reached".into(),
        Blocker::Cooldown(_) => "A reset was used recently; try again later".into(),
        Blocker::Unusable => "No reset is usable right now".into(),
    }
}

fn read_error(error: &ProbeError) -> String {
    match error {
        ProbeError::Unsupported => "Resets need a subscription login".into(),
        ProbeError::Transport => "Could not reach the provider".into(),
        ProbeError::Timeout => "The provider did not answer in time".into(),
        ProbeError::Status(status) => format!("The provider answered HTTP {status}"),
        ProbeError::InvalidResponse => "The provider sent an unexpected answer".into(),
    }
}

fn outcome(answer: Redeemed) -> (ResetOutcome, &'static str) {
    match answer {
        Redeemed::Reset => (ResetOutcome::Reset, "Reset used. Usage limits are cleared."),
        Redeemed::AlreadyUsed => (
            ResetOutcome::AlreadyUsed,
            "The earlier attempt already used this reset. Usage limits are cleared.",
        ),
        Redeemed::NotLimited => (
            ResetOutcome::NotLimited,
            "Nothing used: this reset applies only once a usage limit is reached.",
        ),
        Redeemed::Cooldown => (ResetOutcome::Cooldown, "Nothing used: a reset was used recently."),
        Redeemed::Ineligible => (
            ResetOutcome::Ineligible,
            "Nothing used: the provider does not offer resets to this account.",
        ),
        Redeemed::Unavailable => (ResetOutcome::Unavailable, "Nothing used: no reset is available."),
        Redeemed::RateLimited => (
            ResetOutcome::RateLimited,
            "Nothing used: the provider asked to wait. Try again shortly.",
        ),
        Redeemed::Unauthorized => (
            ResetOutcome::Unauthorized,
            "Nothing used: the provider refused the login. Sign in again.",
        ),
        Redeemed::NotConfirmed => (
            ResetOutcome::NotConfirmed,
            "The provider did not confirm the reset. Refresh quotas to check.",
        ),
    }
}

impl Node {
    fn resets(&self) -> std::sync::MutexGuard<'_, Resets> {
        self.resets.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reset_views(&self) -> BTreeMap<AccountId, ResetsView> {
        self.resets().views.clone()
    }

    pub(crate) fn record_resets(
        &self,
        account: &AccountId,
        at: DateTime<Utc>,
        result: Result<ResetCredits, ProbeError>,
    ) {
        let mut resets = self.resets();
        match result {
            Ok(credits) => {
                resets.views.insert(
                    account.clone(),
                    ResetsView {
                        checked_at: at,
                        available: credits.available,
                        blocked: credits.blocker.as_ref().map(blocked_text),
                        credits: credits
                            .credits
                            .into_iter()
                            .map(|c| ResetCreditView {
                                label: Some(c.label).filter(|l| !l.is_empty()),
                                left: c.left,
                                total: c.total,
                                expires_at: c.expires_at,
                            })
                            .collect(),
                        error: None,
                    },
                );
            }
            // API-key credentials have no resets; say nothing rather than fail.
            Err(ProbeError::Unsupported) => {
                resets.views.remove(account);
            }
            Err(error) => {
                let view = resets.views.entry(account.clone()).or_insert_with(|| ResetsView {
                    checked_at: at,
                    available: 0,
                    blocked: None,
                    credits: Vec::new(),
                    error: None,
                });
                view.error = Some(read_error(&error));
            }
        }
    }

    /// Probes one account's usage and resets and records both.
    pub(crate) async fn refresh_account(
        &self,
        rt: &Runtime,
        config: &Config,
        account: &AccountId,
        credential: &Credential,
    ) {
        let (usage, resets) = match credential.provider.as_str() {
            "claude" => tokio::join!(
                rt.executors.claude.probe_usage(credential, config),
                rt.executors.claude.reset_credits(credential, config)
            ),
            "codex" => tokio::join!(
                rt.executors.codex.probe_usage(credential, config),
                rt.executors.codex.reset_credits(credential, config)
            ),
            _ => (Err(ProbeError::Unsupported), Err(ProbeError::Unsupported)),
        };
        let at = Utc::now();
        let error = match usage {
            Ok(signals) => self
                .quota_observation(
                    &mut self.lock(),
                    None,
                    account,
                    crate::quota::Signals {
                        at,
                        values: signals,
                        source: sap_core::quota::ObservationSource::Probe,
                    },
                )
                .err()
                .map(|_| "Quota persistence failed".to_owned()),
            Err(error) => Some(error.to_string()),
        };
        self.record_poll(account, at, error);
        self.record_resets(account, at, resets);
        self.changed();
    }

    /// This node's credential for `account`, if it has one.
    fn local_credential(&self, rt: &Runtime, account: &AccountId) -> ControlResult<Arc<Credential>> {
        let mapping = self.discover().map_err(crate::views::internal)?;
        rt.store()
            .snapshot()
            .into_iter()
            .find(|c| mapping.get(&c.id).is_some_and(|id| id == account))
            .ok_or_else(|| {
                ControlError::conflict("This node has no login for the account. Use it from the node that has one.")
            })
    }

    pub(crate) fn use_reset_now(&self, account: &AccountId) -> BoxFuture<'_, ControlResult<ResetResult>> {
        let account = account.clone();
        let node = self.self_weak.upgrade();
        Box::pin(async move {
            let node = node.ok_or_else(|| ControlError::internal("Node unavailable"))?;
            // A spend continues if the HTTP caller disconnects, so its outcome is still
            // recorded and the in-progress mark is cleared.
            tokio::spawn(async move { node.spend_reset(account).await })
                .await
                .map_err(|_| ControlError::internal("Reset task failed"))?
        })
    }

    async fn spend_reset(&self, account: AccountId) -> ControlResult<ResetResult> {
        if !self
            .store
            .accounts()
            .map_err(crate::views::internal)?
            .iter()
            .any(|a| a.id == account)
        {
            return Err(ControlError::not_found("Account not found"));
        }
        let rt = self
            .runtime
            .upgrade()
            .ok_or_else(|| ControlError::internal("Proxy runtime unavailable"))?;
        let credential = self.local_credential(&rt, &account)?;
        let (request_id, grant, retry) = {
            let mut resets = self.resets();
            if !resets.spending.insert(account.clone()) {
                return Err(ControlError::conflict(
                    "A reset is already in progress for this account",
                ));
            }
            match resets
                .unanswered
                .remove(&account)
                .filter(|u| u.at.elapsed() < RETRY_WINDOW)
            {
                Some(u) => (u.request_id, u.grant, true),
                None => (new_request_id(), None, false),
            }
        };
        let config = rt.config();
        let result = match credential.provider.as_str() {
            "claude" => {
                rt.executors
                    .claude
                    .redeem_reset(&credential, &config, &request_id, grant.as_deref())
                    .await
            }
            "codex" => rt.executors.codex.redeem_reset(&credential, &config, &request_id).await,
            _ => Err(RedeemError::Probe(ProbeError::Unsupported)),
        };
        let finished = self
            .finish_reset(&rt, &config, &account, &credential, request_id, retry, result)
            .await;
        self.resets().spending.remove(&account);
        finished
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_reset(
        &self,
        rt: &Runtime,
        config: &Config,
        account: &AccountId,
        credential: &Credential,
        request_id: String,
        retry: bool,
        result: Result<(Option<String>, Redeemed), RedeemError>,
    ) -> ControlResult<ResetResult> {
        let (grant, answer) = match result {
            Ok(answered) => answered,
            Err(RedeemError::Probe(error)) => {
                return Err(ControlError::upstream(format!(
                    "Could not check the account's resets: {}",
                    read_error(&error)
                )));
            }
            Err(RedeemError::Blocked(blocker)) => {
                return Err(ControlError::conflict(format!(
                    "No reset used. {}.",
                    blocked_text(&blocker)
                )));
            }
            Err(RedeemError::Unknown { grant }) => {
                self.resets().unanswered.insert(
                    account.clone(),
                    Unanswered {
                        request_id,
                        grant,
                        at: Instant::now(),
                    },
                );
                return Err(ControlError::upstream(
                    "The provider did not answer. Try again within 10 minutes: the retry cannot use a second reset.",
                ));
            }
        };
        // A refusal of a retry does not prove the earlier unanswered request spent nothing.
        if retry && matches!(answer, Redeemed::RateLimited | Redeemed::Unauthorized) {
            self.resets().unanswered.insert(
                account.clone(),
                Unanswered {
                    request_id,
                    grant,
                    at: Instant::now(),
                },
            );
        }
        if matches!(answer, Redeemed::Reset | Redeemed::AlreadyUsed) {
            self.clear_limits(rt, account, credential);
            self.event(
                EventKind::AccountState,
                None,
                Some(account.clone()),
                "Manual usage reset used",
            );
        }
        // Fresh windows and the remaining resets, whatever the answer.
        self.refresh_account(rt, config, account, credential).await;
        let (outcome, message) = outcome(answer);
        let view = crate::control::Control::accounts(self)
            .into_iter()
            .find(|a| &a.id == account)
            .ok_or_else(|| ControlError::not_found("Account not found"))?;
        Ok(ResetResult {
            outcome,
            message: message.into(),
            account: view,
        })
    }

    /// After a confirmed reset: forget exhaustion and cooling for the account and clear
    /// the proxy's cooldowns for its credential, as the management center does.
    fn clear_limits(&self, rt: &Runtime, account: &AccountId, credential: &Credential) {
        rt.store().reset_cooldowns(&credential.id);
        let mut state = self.lock();
        let local = state.accounts.entry(account.clone()).or_default();
        local.exhausted = None;
        local.exhausted_window = None;
        local.cooling = None;
        let crate::node::State { registry, failures, .. } = &mut *state;
        failures.retain(|key, (outcome, _)| {
            !matches!(outcome, Outcome::Exhausted { .. } | Outcome::RateLimited { .. })
                || !registry
                    .get(key)
                    .and_then(|r| r.assignment.as_ref())
                    .is_some_and(|assignment| &assignment.account == account)
        });
        if self.persist_account(&state, account).is_err() {
            tracing::warn!("SAP account state persistence failed after a reset");
        }
    }
}
