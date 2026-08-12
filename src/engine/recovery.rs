use crate::{StoreError, ToolResolver, ToolStatus};

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

pub(crate) async fn recover_tool_statuses(
    store: &crate::PostgresStore,
    tools: &dyn ToolResolver,
) -> Result<RecoveryReport, StoreError> {
    let mut report = RecoveryReport::default();
    for call in store.recoverable_tool_calls(256).await? {
        let tool = tools.bound_tool(call.session_id.clone(), &call.qualified_name, &call.descriptor_digest)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        match tool.status(&call.idempotency_key).await {
            Ok(ToolStatus::Succeeded { result }) => {
                tool.descriptor().output().validate(&result).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                store.finish_tool_call(call.frame_id, call.id, call.revision, Ok(result)).await?;
                report.tools_recovered += 1;
            }
            Ok(ToolStatus::Failed { error }) => {
                store.finish_tool_call(call.frame_id, call.id, call.revision, Err(error)).await?;
                report.tools_recovered += 1;
            }
            Ok(ToolStatus::Pending | ToolStatus::Running | ToolStatus::WaitingForPermission) => {
                report.tools_requeued += 1;
            }
            Err(_) => report.outcomes_unknown += 1,
        }
    }
    Ok(report)
}
