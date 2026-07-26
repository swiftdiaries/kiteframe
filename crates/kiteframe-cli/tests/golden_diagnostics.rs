use kiteframe_contract::{Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage};

fn fixture_diagnostic(code: DiagnosticCode) -> Diagnostic {
    let (category, stage, message) = match code {
        DiagnosticCode::PackageInvalid => (
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "package input is invalid",
        ),
        DiagnosticCode::PackageContainment => (
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "package input escapes its boundary",
        ),
        DiagnosticCode::LockStale => (
            DiagnosticCategory::Lock,
            DiagnosticStage::Resolve,
            "lock input is stale",
        ),
        DiagnosticCode::LockTampered => (
            DiagnosticCategory::Lock,
            DiagnosticStage::Resolve,
            "lock input failed integrity validation",
        ),
        DiagnosticCode::CatalogIncompatible => (
            DiagnosticCategory::Catalog,
            DiagnosticStage::Lock,
            "catalog has no compatible candidate",
        ),
        DiagnosticCode::FeatureUnsupported => (
            DiagnosticCategory::Feature,
            DiagnosticStage::Resolve,
            "required feature is unsupported",
        ),
        DiagnosticCode::AdmissionDenied => (
            DiagnosticCategory::Authorization,
            DiagnosticStage::Admit,
            "admission was denied",
        ),
        DiagnosticCode::AdmissionExpired => (
            DiagnosticCategory::Authorization,
            DiagnosticStage::Admit,
            "admission evidence expired",
        ),
        DiagnosticCode::InvocationDenied => (
            DiagnosticCategory::Authorization,
            DiagnosticStage::Invoke,
            "invocation was denied",
        ),
        DiagnosticCode::PolicyStale => (
            DiagnosticCategory::Authorization,
            DiagnosticStage::Invoke,
            "authorization policy is stale",
        ),
        DiagnosticCode::PreconditionMissing => (
            DiagnosticCategory::Capability,
            DiagnosticStage::Invoke,
            "capability precondition is missing",
        ),
        DiagnosticCode::ResultInvalid => (
            DiagnosticCategory::Capability,
            DiagnosticStage::Invoke,
            "capability result is invalid",
        ),
        DiagnosticCode::OutcomeUnknown => (
            DiagnosticCategory::Capability,
            DiagnosticStage::Invoke,
            "capability outcome is unknown",
        ),
        DiagnosticCode::AuditUnavailable => (
            DiagnosticCategory::Audit,
            DiagnosticStage::Audit,
            "audit service is unavailable",
        ),
        DiagnosticCode::ComponentUnresolved => (
            DiagnosticCategory::Runtime,
            DiagnosticStage::Resolve,
            "runtime component is unresolved",
        ),
        DiagnosticCode::RuntimeConstruction => (
            DiagnosticCategory::Runtime,
            DiagnosticStage::Runtime,
            "runtime construction failed",
        ),
    };

    Diagnostic::error(code, category, stage, message)
}

fn render_reserved_diagnostic_fixture() -> String {
    let diagnostics = DiagnosticCode::ALL
        .iter()
        .copied()
        .map(fixture_diagnostic)
        .collect::<Vec<_>>();
    let mut rendered = serde_json_canonicalizer::to_string(&diagnostics).unwrap();
    rendered.push('\n');
    rendered
}

#[test]
fn all_diagnostic_codes_have_redacted_json_fixtures() {
    let actual = render_reserved_diagnostic_fixture();
    let expected = include_str!("fixtures/diagnostics.json");

    assert_eq!(actual, expected);
    let values: serde_json::Value = serde_json::from_str(&actual).unwrap();
    for diagnostic in values.as_array().unwrap() {
        let object = diagnostic.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "category",
                "code",
                "details",
                "help",
                "message",
                "package_path",
                "retry",
                "severity",
                "source_range",
                "stage",
            ]
        );
        assert_eq!(object["details"], serde_json::json!({}));
        assert!(object["help"].is_null());
        assert!(object["package_path"].is_null());
        assert!(object["source_range"].is_null());
    }
}
