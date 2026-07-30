use std::time::{SystemTime, UNIX_EPOCH};

use kiteframe_contract::{
    AuthorityRevisionSet, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    Timestamp,
};
use kiteframe_provider::AuthenticatedInvocationContext;

pub(crate) fn current_timestamp(stage: DiagnosticStage) -> Result<Timestamp, Diagnostic> {
    timestamp_from(SystemTime::now(), stage)
}

fn timestamp_from(value: SystemTime, stage: DiagnosticStage) -> Result<Timestamp, Diagnostic> {
    let seconds = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| stale(stage, "deployment clock is invalid"))?
        .as_secs();
    Ok(Timestamp::new(seconds))
}

pub(crate) fn require_fresh_authority(
    principals: &AuthenticatedInvocationContext,
    loaded: &AuthorityRevisionSet,
    current: &AuthorityRevisionSet,
    now: Timestamp,
    stage: DiagnosticStage,
) -> Result<(), Diagnostic> {
    if loaded != current || principals.expires_at() <= now {
        return Err(stale(
            stage,
            "OpenFGA policy or authenticated principal context is stale",
        ));
    }
    Ok(())
}

fn stale(stage: DiagnosticStage, message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        stage,
        message,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn deployment_clock_failure_preserves_admission_stage() {
        let before_epoch = UNIX_EPOCH.checked_sub(Duration::from_secs(1)).unwrap();

        let error = timestamp_from(before_epoch, DiagnosticStage::Admit).unwrap_err();

        assert_eq!(error.stage, DiagnosticStage::Admit);
        assert_eq!(error.code.as_str(), "KF-AUTH-004");
    }
}
