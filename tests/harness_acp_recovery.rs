use kiteframe::HarnessRecoveryReport;

#[test]
fn harness_recovery_report_counts_each_durable_classification() {
    let report = HarnessRecoveryReport {
        retryable_pre_accept: 2,
        outcome_unknown: 3,
        terminal: 5,
    };

    assert_eq!(report.retryable_pre_accept, 2);
    assert_eq!(report.outcome_unknown, 3);
    assert_eq!(report.terminal, 5);
}
