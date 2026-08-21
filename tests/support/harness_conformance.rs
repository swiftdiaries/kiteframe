use kiteframe::{GoalPhase, HarnessTerminal};

pub enum HarnessConformanceCase {
    Complete,
    Replay,
    RejectPermission,
    CancelInFlight,
    InvalidCompletion,
}

pub struct HarnessConformanceObservation {
    pub terminal: HarnessTerminal,
    pub terminal_count: u64,
    pub goal_phase: GoalPhase,
}

#[async_trait::async_trait]
pub trait HarnessConformanceDriver {
    async fn run(&mut self, case: HarnessConformanceCase) -> HarnessConformanceObservation;
}

fn terminal_code(terminal: &HarnessTerminal) -> Option<&str> {
    match terminal {
        HarnessTerminal::Cancelled => Some("harness_cancelled"),
        HarnessTerminal::Retryable(failure)
        | HarnessTerminal::Failed(failure)
        | HarnessTerminal::OutcomeUnknown(failure) => Some(failure.code),
        HarnessTerminal::Completed(_) => None,
    }
}

pub async fn assert_normative_harness(mut harness: impl HarnessConformanceDriver) {
    let completed = harness.run(HarnessConformanceCase::Complete).await;
    assert!(matches!(completed.terminal, HarnessTerminal::Completed(_)));
    assert_eq!(completed.goal_phase, GoalPhase::Active);

    let replay = harness.run(HarnessConformanceCase::Replay).await;
    assert_eq!(replay.terminal_count, 1);

    let rejected = harness.run(HarnessConformanceCase::RejectPermission).await;
    assert_eq!(
        terminal_code(&rejected.terminal),
        Some("harness_permission_rejected")
    );

    let cancelled = harness.run(HarnessConformanceCase::CancelInFlight).await;
    assert_eq!(
        terminal_code(&cancelled.terminal),
        Some("harness_cancelled")
    );

    let invalid = harness.run(HarnessConformanceCase::InvalidCompletion).await;
    assert_eq!(terminal_code(&invalid.terminal), Some("harness_failed"));
    assert_eq!(invalid.goal_phase, GoalPhase::Active);
}
