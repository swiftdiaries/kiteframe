use super::super::{HarnessApprovalDecision, HarnessFailure, PermissionOption, PermissionOptionId};

pub fn select_permission_once(
    options: &[PermissionOption],
    decision: HarnessApprovalDecision,
) -> Result<PermissionOptionId, HarnessFailure> {
    let wanted = match decision {
        HarnessApprovalDecision::AllowOnce => "allow_once",
        HarnessApprovalDecision::RejectOnce => "reject_once",
    };
    options
        .iter()
        .find(|option| option.kind == wanted)
        .map(|option| option.id.clone())
        .ok_or_else(|| {
            HarnessFailure::new(
                "harness_capability_unsupported",
                "ACP agent did not offer the required one-shot permission option",
            )
        })
}
