use std::time::{SystemTime, UNIX_EPOCH};

use kiteframe_contract::{
    AuthorityRevisionSet, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    Timestamp,
};
use kiteframe_provider::AuthenticatedInvocationContext;

pub(crate) fn current_timestamp() -> Result<Timestamp, Diagnostic> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| stale(DiagnosticStage::Invoke, "deployment clock is invalid"))?
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
