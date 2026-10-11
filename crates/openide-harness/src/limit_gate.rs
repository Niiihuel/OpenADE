//! Process-wide record of provider limits that are known to be closed.
//!
//! Every node of an agent tree asks before a model request. A blocked key
//! pauses the node without sending, so one refusal pauses the whole tree
//! once instead of once per agent.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use openide_core::TurnPauseClass;

use crate::{FailureClass, ProviderFailure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitBlock {
    pub class: TurnPauseClass,
    /// None: closed until someone continues by hand.
    pub resets_at_ms: Option<u64>,
    pub detail: String,
}

fn gate() -> &'static Mutex<HashMap<String, LimitBlock>> {
    static GATE: OnceLock<Mutex<HashMap<String, LimitBlock>>> = OnceLock::new();
    GATE.get_or_init(Default::default)
}

/// The pause a failure leaves the turn in, or None when the turn fails.
pub(crate) fn pause_class(failure: &ProviderFailure) -> Option<TurnPauseClass> {
    Some(match failure.class {
        FailureClass::Transient => TurnPauseClass::Transient,
        FailureClass::RateLimited | FailureClass::LimitReached => TurnPauseClass::LimitReached,
        FailureClass::OutOfCredits => TurnPauseClass::OutOfCredits,
        FailureClass::Auth => TurnPauseClass::Auth,
        FailureClass::ContextTooLarge | FailureClass::Fatal => return None,
    })
}

/// The pause to record before sending the request of `step`: a paused child
/// this turn was waiting for, or a limit already known to be closed.
pub(crate) fn pause_before_request(
    model: &dyn crate::ModelBackend,
    messages: &[openide_core::AgentMessage],
    step: usize,
    continuing_past_child: bool,
) -> Option<openide_core::AgentEventKind> {
    let account_key = model.limit_key().unwrap_or_default();
    let family_key = family_key(model);
    let limit_key = account_key.clone();
    let waiting = (!continuing_past_child)
        .then(|| crate::subagent_tools::paused_child_in_last_batch(messages))
        .flatten();
    if let Some((detail, resets_at_ms)) = waiting {
        return Some(openide_core::AgentEventKind::TurnPaused {
            class: TurnPauseClass::WaitingOnChild,
            limit_key,
            resets_at_ms,
            step,
            detail,
        });
    }
    if account_key.is_empty() {
        return None;
    }
    // The account first (no credit, no credential), then this model's own.
    let now = crate::provider_failure::now_ms();
    let (limit_key, block) = [Some(account_key), family_key]
        .into_iter()
        .flatten()
        .find_map(|key| limit_gate_check(&key, now).map(|block| (key, block)))?;
    Some(openide_core::AgentEventKind::TurnPaused {
        class: block.class,
        limit_key,
        resets_at_ms: block.resets_at_ms,
        step,
        detail: block.detail,
    })
}

/// The pause a failed request of `step` leaves behind, closing the shared
/// gate for the classes that outlast one request. None: the turn fails.
pub(crate) fn pause_for_failure(
    model: &dyn crate::ModelBackend,
    failure: &ProviderFailure,
    step: usize,
) -> Option<openide_core::AgentEventKind> {
    let class = pause_class(failure)?;
    // A usage limit is this model's where the provider counts per model;
    // credit and credentials belong to the account.
    let limit_key = family_key(model)
        .filter(|_| class == TurnPauseClass::LimitReached)
        .or_else(|| model.limit_key())
        .unwrap_or_default();
    if !limit_key.is_empty() {
        limit_gate_block(
            &limit_key,
            LimitBlock {
                class,
                resets_at_ms: failure.resets_at_ms,
                detail: failure.message.clone(),
            },
        );
    }
    Some(openide_core::AgentEventKind::TurnPaused {
        class,
        limit_key,
        resets_at_ms: failure.resets_at_ms,
        step,
        detail: failure.message.clone(),
    })
}

/// Whether a request of `model` would be held right now by a closed limit
/// of its account or of its model family.
pub fn limit_gate_blocked(model: &dyn crate::ModelBackend) -> bool {
    pause_before_request(model, &[], 0, true).is_some()
}

/// The gate key of this backend's model family, where its provider has one.
fn family_key(model: &dyn crate::ModelBackend) -> Option<String> {
    let account = model.limit_key().filter(|key| !key.is_empty())?;
    Some(format!("{account}:{}", model.limit_family()?))
}

/// Close `key`. A transient failure closes nothing: the next request may work.
pub fn limit_gate_block(key: &str, block: LimitBlock) {
    if block.class != TurnPauseClass::Transient && block.class != TurnPauseClass::WaitingOnChild {
        gate()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(key.to_owned(), block);
    }
}

/// The block still in force for `key`. One whose reset has passed is dropped.
pub fn limit_gate_check(key: &str, now_ms: u64) -> Option<LimitBlock> {
    let mut gate = gate().lock().unwrap_or_else(|poison| poison.into_inner());
    if gate
        .get(key)?
        .resets_at_ms
        .is_some_and(|at| at <= now_ms)
    {
        gate.remove(key);
    }
    gate.get(key).cloned()
}

/// A person chose to continue now: let the next request through.
pub fn limit_gate_clear(key: &str) {
    gate()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Route(&'static str, Option<&'static str>);

    #[async_trait::async_trait]
    impl crate::ModelBackend for Route {
        fn model_id(&self) -> &str {
            "m"
        }
        fn limit_key(&self) -> Option<String> {
            Some(self.0.into())
        }
        fn limit_family(&self) -> Option<String> {
            self.1.map(str::to_owned)
        }
        async fn complete(
            &self,
            _messages: &[openide_core::AgentMessage],
            _tools: &[crate::ToolDefinition],
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<crate::ModelOutput, crate::AgentError> {
            unreachable!()
        }
    }

    #[test]
    fn a_usage_limit_holds_its_model_family_and_credit_holds_the_account() {
        let fail = |class| ProviderFailure::new(class, "x");
        let held = |route: &Route| pause_before_request(route, &[], 1, false).is_some();
        let (opus, sonnet) = (Route("family-account", Some("opus")), Route("family-account", Some("sonnet")));
        // One model's limit does not hold the other on the same account.
        let Some(openide_core::AgentEventKind::TurnPaused { limit_key, .. }) =
            pause_for_failure(&opus, &fail(FailureClass::LimitReached), 1)
        else {
            panic!()
        };
        assert_eq!(limit_key, "family-account:opus");
        assert!(held(&opus) && !held(&sonnet));
        // Continuing clears exactly the key the pause recorded.
        limit_gate_clear(&limit_key);
        assert!(!held(&opus));
        // No credit is the account's: every family on it is held.
        pause_for_failure(&opus, &fail(FailureClass::OutOfCredits), 1).unwrap();
        assert!(held(&opus) && held(&sonnet));
        limit_gate_clear("family-account");
        // A provider with one limit for everything keeps the account key.
        let (plan_a, plan_b) = (Route("family-plan", None), Route("family-plan", None));
        pause_for_failure(&plan_a, &fail(FailureClass::LimitReached), 1).unwrap();
        assert!(held(&plan_b));
        limit_gate_clear("family-plan");
    }

    #[test]
    fn a_block_holds_until_its_reset_or_a_manual_clear() {
        let block = |class, resets_at_ms| LimitBlock {
            class,
            resets_at_ms,
            detail: "x".into(),
        };
        limit_gate_block("gate-test-known", block(TurnPauseClass::LimitReached, Some(5_000)));
        assert!(limit_gate_check("gate-test-known", 4_999).is_some());
        assert!(limit_gate_check("gate-test-known", 5_000).is_none());
        assert!(limit_gate_check("gate-test-known", 1).is_none(), "an expired block is gone");

        limit_gate_block("gate-test-credits", block(TurnPauseClass::OutOfCredits, None));
        assert!(limit_gate_check("gate-test-credits", u64::MAX).is_some());
        limit_gate_clear("gate-test-credits");
        assert!(limit_gate_check("gate-test-credits", 0).is_none());

        limit_gate_block("gate-test-transient", block(TurnPauseClass::Transient, None));
        assert!(limit_gate_check("gate-test-transient", 0).is_none());
    }
}
