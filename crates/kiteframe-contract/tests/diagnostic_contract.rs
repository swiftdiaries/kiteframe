use kiteframe_contract::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity, DiagnosticStage, RetryClass,
};

#[test]
fn stable_code_serializes_as_reserved_wire_value() {
    let diagnostic = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        "manifest is invalid",
    );
    let json = serde_json::to_value(diagnostic.clone()).unwrap();
    assert_eq!(json["code"], "KF-PKG-001");
    assert_eq!(json["retry"], "never");
    assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
    assert_eq!(diagnostic.retry, RetryClass::Never);
    assert!(json.get("details").is_some());
}

#[test]
fn diagnostics_sort_by_stage_path_range_then_code() {
    let mut diagnostics = [
        Diagnostic::error(
            DiagnosticCode::LockStale,
            DiagnosticCategory::Lock,
            DiagnosticStage::Lock,
            "lock is stale",
        ),
        Diagnostic::error(
            DiagnosticCode::PackageInvalid,
            DiagnosticCategory::Package,
            DiagnosticStage::Parse,
            "manifest is invalid",
        ),
    ];
    diagnostics.sort();
    assert_eq!(diagnostics[0].stage, DiagnosticStage::Parse);
}
