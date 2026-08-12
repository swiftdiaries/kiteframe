use std::collections::BTreeSet;

use serde_json::json;

use crate::{StoreError, ToolError, ToolResolver, ToolStatus};

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
    Ok(RecoveryReport {
        model_attempts_requeued: store.requeue_uncommitted_model_attempts(256).await?,
        ..RecoveryReport::default()
    })
}

pub(crate) async fn recover_tool_statuses(
    store: &crate::PostgresStore,
    tools: &dyn ToolResolver,
    status_poll_retry_budget: u32,
) -> Result<RecoveryReport, StoreError> {
    let mut report = RecoveryReport::default();
    let mut sessions_waiting_for_mcp = BTreeSet::new();
    for call in store.recoverable_tool_calls(256).await? {
        let tool = match tools.bound_tool(
            call.session_id.clone(),
            &call.qualified_name,
            &call.descriptor_digest,
        ) {
            Ok(tool) => tool,
            Err(ToolError::UnknownSession(_)) => {
                sessions_waiting_for_mcp.insert(call.session_id);
                continue;
            }
            Err(ToolError::ToolContractChanged { .. } | ToolError::UnknownTool(_)) => {
                store
                    .finish_tool_call(
                        call.frame_id,
                        call.id,
                        call.revision,
                        Err(json!({"code":"tool_contract_changed"})),
                    )
                    .await?;
                report.tools_recovered += 1;
                continue;
            }
            Err(_) => {
                store
                    .mark_tool_call_outcome_unknown(call.frame_id, call.id, call.revision)
                    .await?;
                report.outcomes_unknown += 1;
                continue;
            }
        };
        let Some(revision) = store
            .begin_tool_status_check(
                call.frame_id.clone(),
                call.id,
                call.revision,
                status_poll_retry_budget,
            )
            .await?
        else {
            store
                .mark_tool_call_outcome_unknown(call.frame_id, call.id, call.revision)
                .await?;
            report.outcomes_unknown += 1;
            continue;
        };
        match tool.status(&call.idempotency_key).await {
            Ok(ToolStatus::Succeeded { result }) => {
                tool.descriptor()
                    .output()
                    .validate(&result)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                store
                    .finish_tool_call(call.frame_id, call.id, revision, Ok(result))
                    .await?;
                report.tools_recovered += 1;
            }
            Ok(ToolStatus::Failed { error }) => {
                store
                    .finish_tool_call(call.frame_id, call.id, revision, Err(error))
                    .await?;
                report.tools_recovered += 1;
            }
            Ok(ToolStatus::Pending | ToolStatus::Running | ToolStatus::WaitingForPermission) => {
                report.tools_requeued += 1;
            }
            Ok(ToolStatus::NotStarted | ToolStatus::Unknown) | Err(_) => {
                store
                    .mark_tool_call_outcome_unknown(call.frame_id, call.id, revision)
                    .await?;
                report.outcomes_unknown += 1;
            }
        }
    }
    report.sessions_waiting_for_mcp = sessions_waiting_for_mcp.len() as u64;
    Ok(report)
}
