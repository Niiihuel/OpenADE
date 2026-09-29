//! Deterministic tool-batch scheduling plan.
//!
//! The host, after catalog validation and permission resolution, declares an
//! effect for every call. Consecutive trusted reads form one bounded concurrent
//! wave. Every mutation is a singleton barrier. MCP calls and the built-in
//! write/command tools remain exclusive even if mislabeled as reads.
//!
//! The runner syncs `ToolStarted` for every call in a wave before executing
//! any of them. Replay treats any unfinished start as uncertain after a crash:
//! automatic recovery never repeats that call. A start followed by a crash
//! before execution therefore still requires manual verification. Results are
//! committed in model order, even if reads finish in another order.

use crate::{AgentError, ToolContext, ToolExecutor, ToolOutput};
use futures_util::stream::{self, StreamExt};
use openide_core::ToolCall;
use std::collections::HashSet;

const MAX_PARALLEL_READS: usize = 4;

pub(crate) fn trusted_local_read(name: &str) -> bool {
    matches!(name, "read_file" | "list_files" | "search_files")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolEffect {
    ReadOnly,
    Mutating,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledCall {
    pub call: ToolCall,
    /// Assigned by trusted host policy, never the model or an MCP hint alone.
    pub effect: ToolEffect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScheduleGroup {
    ReadWave(Vec<usize>),
    Mutation(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct ToolScheduler {
    max_parallel_reads: usize,
}

impl Default for ToolScheduler {
    fn default() -> Self {
        Self {
            max_parallel_reads: 4,
        }
    }
}

/// Evidence supplied by the runner after every ordered `ToolStarted` in the
/// wave was synced to the native journal. It is never derived from model input.
#[derive(Clone, Debug)]
pub(crate) struct ReadWaveAdmission {
    sequence: u64,
    call_ids: Vec<String>,
}

impl ToolScheduler {
    pub fn new(max_parallel_reads: usize) -> Result<Self, AgentError> {
        if !(1..=MAX_PARALLEL_READS).contains(&max_parallel_reads) {
            return Err(AgentError::InvalidRequest(format!(
                "parallel read limit must be between 1 and {MAX_PARALLEL_READS}"
            )));
        }
        Ok(Self { max_parallel_reads })
    }

    /// Split one model batch in its original order. A later read cannot pass
    /// an earlier mutation, and an MCP call is always a singleton barrier.
    pub fn plan(&self, calls: &[ScheduledCall]) -> Result<Vec<ScheduleGroup>, AgentError> {
        let mut groups = Vec::new();
        let mut read_wave = Vec::new();
        let mut ids = HashSet::new();
        for (index, scheduled) in calls.iter().enumerate() {
            if scheduled.call.id.is_empty()
                || scheduled.call.name.is_empty()
                || !ids.insert(scheduled.call.id.as_str())
            {
                return Err(AgentError::InvalidRequest(
                    "tool batch contains an empty or duplicate call identity".into(),
                ));
            }
            match effective_effect(scheduled) {
                ToolEffect::ReadOnly => {
                    read_wave.push(index);
                    if read_wave.len() == self.max_parallel_reads {
                        groups.push(ScheduleGroup::ReadWave(std::mem::take(&mut read_wave)));
                    }
                }
                ToolEffect::Mutating => {
                    if !read_wave.is_empty() {
                        groups.push(ScheduleGroup::ReadWave(std::mem::take(&mut read_wave)));
                    }
                    groups.push(ScheduleGroup::Mutation(index));
                }
            }
        }
        if !read_wave.is_empty() {
            groups.push(ScheduleGroup::ReadWave(read_wave));
        }
        Ok(groups)
    }

    /// Execute one already-admitted read wave with bounded concurrency.
    /// Results are returned in model-call order even when completion order
    /// differs. The caller must append durable results in this order.
    pub async fn execute_read_wave(
        &self,
        calls: &[ScheduledCall],
        group: &ScheduleGroup,
        admission: ReadWaveAdmission,
        executor: &dyn ToolExecutor,
        context: &ToolContext,
    ) -> Result<Vec<ToolOutput>, AgentError> {
        let ScheduleGroup::ReadWave(indices) = group else {
            return Err(AgentError::InvalidRequest(
                "only read waves can execute concurrently".into(),
            ));
        };
        if !self.plan(calls)?.contains(group) {
            return Err(AgentError::InvalidRequest(
                "read wave crosses a mutation barrier or omits calls".into(),
            ));
        }
        if admission.sequence == 0
            || indices.is_empty()
            || admission.call_ids.len() != indices.len()
        {
            return Err(AgentError::InvalidRequest(
                "read wave lacks a matching durable admission".into(),
            ));
        }
        let mut previous = None;
        for (position, &index) in indices.iter().enumerate() {
            let Some(scheduled) = calls.get(index) else {
                return Err(AgentError::InvalidRequest(
                    "read wave index is invalid".into(),
                ));
            };
            if previous.is_some_and(|prior| index <= prior)
                || effective_effect(scheduled) != ToolEffect::ReadOnly
                || admission.call_ids[position] != scheduled.call.id
            {
                return Err(AgentError::InvalidRequest(
                    "read wave admission or effect does not match its calls".into(),
                ));
            }
            previous = Some(index);
        }
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let mut pending = stream::iter(indices.iter().copied().enumerate().map(
            |(position, index)| async move {
                let result = executor.execute(&calls[index].call, context).await;
                (position, result)
            },
        ))
        .buffer_unordered(self.max_parallel_reads);
        let mut ordered = vec![None; indices.len()];
        while !ordered.iter().all(Option::is_some) {
            let next = tokio::select! {
                biased;
                _ = context.cancel.cancelled() => return Err(AgentError::Cancelled),
                next = pending.next() => next,
            };
            let (position, result) = next.ok_or_else(|| {
                AgentError::Runtime("read wave ended before all results arrived".into())
            })?;
            ordered[position] = Some(match result {
                Ok(output) => output,
                Err(error @ (AgentError::Cancelled | AgentError::EffectUncertain(_))) => {
                    return Err(error);
                }
                Err(error) => ToolOutput::error(error.to_string()),
            });
        }
        ordered
            .into_iter()
            .map(|output| {
                output.ok_or_else(|| AgentError::Runtime("read wave result is missing".into()))
            })
            .collect()
    }
}

fn effective_effect(call: &ScheduledCall) -> ToolEffect {
    if !trusted_local_read(&call.call.name) {
        ToolEffect::Mutating
    } else {
        call.effect
    }
}

impl ReadWaveAdmission {
    pub fn after_persisted_starts(
        sequence: u64,
        calls: &[ScheduledCall],
        indices: &[usize],
    ) -> Self {
        Self {
            sequence,
            call_ids: indices
                .iter()
                .map(|index| calls[*index].call.id.clone())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn call(id: &str, name: &str, effect: ToolEffect) -> ScheduledCall {
        ScheduledCall {
            call: ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: json!({}),
            },
            effect,
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            workspace: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn partitions_reads_around_every_exclusive_effect() {
        let calls = vec![
            call("r1", "read_file", ToolEffect::ReadOnly),
            call("r2", "search_files", ToolEffect::ReadOnly),
            call("w", "write_file", ToolEffect::ReadOnly),
            call("m", "mcp__server__read", ToolEffect::ReadOnly),
            call("r3", "list_files", ToolEffect::ReadOnly),
            call("c", "run_command", ToolEffect::ReadOnly),
            call("r4", "read_file", ToolEffect::ReadOnly),
        ];
        assert_eq!(
            ToolScheduler::default().plan(&calls).unwrap(),
            vec![
                ScheduleGroup::ReadWave(vec![0, 1]),
                ScheduleGroup::Mutation(2),
                ScheduleGroup::Mutation(3),
                ScheduleGroup::ReadWave(vec![4]),
                ScheduleGroup::Mutation(5),
                ScheduleGroup::ReadWave(vec![6]),
            ]
        );
    }

    #[test]
    fn rejects_duplicate_calls_and_unbounded_parallelism() {
        let calls = vec![
            call("same", "read_file", ToolEffect::ReadOnly),
            call("same", "read_file", ToolEffect::ReadOnly),
        ];
        assert!(ToolScheduler::default().plan(&calls).is_err());
        assert!(ToolScheduler::new(0).is_err());
        assert!(ToolScheduler::new(MAX_PARALLEL_READS + 1).is_err());
    }

    struct ReadExecutor {
        active: AtomicUsize,
        peak: AtomicUsize,
        started: AtomicUsize,
        delay_ms: u64,
    }

    #[async_trait]
    impl ToolExecutor for ReadExecutor {
        fn definitions(&self) -> Vec<crate::ToolDefinition> {
            Vec::new()
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _active = Active(&self.active);
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.started.fetch_add(1, Ordering::SeqCst);
            let delay = if call.id == "slow" {
                self.delay_ms * 2
            } else {
                self.delay_ms
            };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            Ok(ToolOutput::success(call.id.clone()))
        }
    }

    #[tokio::test]
    async fn parallel_reads_complete_out_of_order_but_return_in_model_order() {
        let calls = vec![
            call("slow", "read_file", ToolEffect::ReadOnly),
            call("fast", "list_files", ToolEffect::ReadOnly),
        ];
        let scheduler = ToolScheduler::new(2).unwrap();
        let group = scheduler.plan(&calls).unwrap().remove(0);
        let ScheduleGroup::ReadWave(indices) = &group else {
            panic!("expected reads")
        };
        let executor = ReadExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            delay_ms: 20,
        };
        let results = scheduler
            .execute_read_wave(
                &calls,
                &group,
                ReadWaveAdmission::after_persisted_starts(1, &calls, indices),
                &executor,
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(
            results
                .iter()
                .map(|output| output.content.as_str())
                .collect::<Vec<_>>(),
            vec!["slow", "fast"]
        );
        assert_eq!(executor.peak.load(Ordering::SeqCst), 2);
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_read_count_is_bounded() {
        let calls = (0..5)
            .map(|index| call(&format!("r{index}"), "read_file", ToolEffect::ReadOnly))
            .collect::<Vec<_>>();
        let scheduler = ToolScheduler::new(2).unwrap();
        let groups = scheduler.plan(&calls).unwrap();
        assert_eq!(groups.len(), 3);
        let executor = ReadExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            delay_ms: 15,
        };
        let mut outputs = Vec::new();
        for group in &groups {
            let ScheduleGroup::ReadWave(indices) = group else {
                panic!("expected reads")
            };
            outputs.extend(
                scheduler
                    .execute_read_wave(
                        &calls,
                        group,
                        ReadWaveAdmission::after_persisted_starts(2, &calls, indices),
                        &executor,
                        &context(),
                    )
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(outputs.len(), 5);
        assert_eq!(executor.started.load(Ordering::SeqCst), 5);
        assert_eq!(executor.peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancellation_drops_all_active_read_futures() {
        let calls = vec![
            call("slow", "read_file", ToolEffect::ReadOnly),
            call("fast", "list_files", ToolEffect::ReadOnly),
        ];
        let scheduler = ToolScheduler::new(2).unwrap();
        let group = scheduler.plan(&calls).unwrap().remove(0);
        let ScheduleGroup::ReadWave(indices) = &group else {
            panic!("expected reads")
        };
        let admission = ReadWaveAdmission::after_persisted_starts(9, &calls, indices);
        let executor = Arc::new(ReadExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            delay_ms: 30_000,
        });
        let context = context();
        let cancel = context.cancel.clone();
        let worker = {
            let executor = Arc::clone(&executor);
            tokio::spawn(async move {
                scheduler
                    .execute_read_wave(&calls, &group, admission, executor.as_ref(), &context)
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while executor.started.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        assert!(matches!(worker.await.unwrap(), Err(AgentError::Cancelled)));
        assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn admission_must_match_and_mutations_never_enter_read_executor() {
        let calls = vec![
            call("read", "read_file", ToolEffect::ReadOnly),
            call("write", "write_file", ToolEffect::ReadOnly),
            call("later", "read_file", ToolEffect::ReadOnly),
        ];
        let scheduler = ToolScheduler::default();
        let executor = ReadExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            delay_ms: 1,
        };
        let forged = ScheduleGroup::ReadWave(vec![0, 1]);
        let result = scheduler
            .execute_read_wave(
                &calls,
                &forged,
                ReadWaveAdmission::after_persisted_starts(1, &calls, &[0, 1]),
                &executor,
                &context(),
            )
            .await;
        assert!(matches!(result, Err(AgentError::InvalidRequest(_))));
        let across_barrier = ScheduleGroup::ReadWave(vec![0, 2]);
        let result = scheduler
            .execute_read_wave(
                &calls,
                &across_barrier,
                ReadWaveAdmission::after_persisted_starts(2, &calls, &[0, 2]),
                &executor,
                &context(),
            )
            .await;
        assert!(matches!(result, Err(AgentError::InvalidRequest(_))));
        assert_eq!(executor.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn uncertain_read_outcome_is_not_turned_into_a_committed_error_result() {
        struct UncertainRead;
        #[async_trait]
        impl ToolExecutor for UncertainRead {
            fn definitions(&self) -> Vec<crate::ToolDefinition> {
                Vec::new()
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                Err(AgentError::EffectUncertain(
                    "lost response after read".into(),
                ))
            }
        }
        let calls = vec![call("a", "read_file", ToolEffect::ReadOnly)];
        let scheduler = ToolScheduler::default();
        let group = scheduler.plan(&calls).unwrap().remove(0);
        let ScheduleGroup::ReadWave(indices) = &group else {
            panic!("expected wave")
        };
        let result = scheduler
            .execute_read_wave(
                &calls,
                &group,
                ReadWaveAdmission::after_persisted_starts(1, &calls, indices),
                &UncertainRead,
                &context(),
            )
            .await;
        assert!(matches!(result, Err(AgentError::EffectUncertain(_))));
    }
}
