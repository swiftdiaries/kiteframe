use crate::StoreError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub model_attempts_requeued: u64,
    pub tools_recovered: u64,
    pub tools_requeued: u64,
    pub sessions_waiting_for_mcp: u64,
    pub outcomes_unknown: u64,
}

pub(crate) async fn recover_model_attempts(
    store: &crate::PostgresStore,
) -> Result<RecoveryReport, StoreError> {
    Ok(RecoveryReport { model_attempts_requeued: store.requeue_uncommitted_model_attempts(256).await?, ..RecoveryReport::default() })
}
