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
    let limit_key = model.limit_key().unwrap_or_default();
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
    if limit_key.is_empty() {
        return None;
    }
    let block = limit_gate_check(&limit_key, crate::provider_failure::now_ms())?;
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
    let limit_key = model.limit_key().unwrap_or_default();
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
