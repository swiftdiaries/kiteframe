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
fn compile_output_has_a_dedicated_stable_wire_code() {
    let diagnostic = Diagnostic::error(
        DiagnosticCode::CompileOutput,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        "compiled IR output cannot be written",
    );

    assert_eq!(diagnostic.code.as_str(), "KF-CLI-001");
    assert_eq!(
        serde_json::to_value(diagnostic).unwrap()["code"],
        "KF-CLI-001"
    );
}

#[test]
fn lock_output_has_a_dedicated_stable_wire_code() {
    let diagnostic = Diagnostic::error(
        DiagnosticCode::LockOutput,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Lock,
        "capability lock output cannot be written",
    );

    assert_eq!(diagnostic.code.as_str(), "KF-CLI-002");
    assert_eq!(
        serde_json::to_value(diagnostic).unwrap()["code"],
        "KF-CLI-002"
    );
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

#[test]
fn diagnostics_with_the_same_ordering_key_are_equal() {
    let first = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        "first safe message",
    );
    let second = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        "second safe message",
    );

    assert_eq!(first.cmp(&second), std::cmp::Ordering::Equal);
    assert_eq!(first, second);
}

#[test]
fn diagnostic_code_and_safe_message_expose_string_slices_without_display() {
    let diagnostic = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        "manifest is invalid",
    );

    assert_eq!(diagnostic.code.as_str(), "KF-PKG-001");
    assert_eq!(diagnostic.message.as_str(), "manifest is invalid");
}
