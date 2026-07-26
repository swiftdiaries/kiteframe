use jsonschema::draft202012;
use kiteframe_contract::{
    CapabilityDescriptor, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
};

pub(crate) fn validate_descriptor(descriptor: &CapabilityDescriptor) -> Result<(), Diagnostic> {
    validate_schema(descriptor.input_schema().as_value(), "input")?;
    validate_schema(descriptor.output_schema().as_value(), "output")
}

fn validate_schema(schema: &serde_json::Value, role: &str) -> Result<(), Diagnostic> {
    draft202012::options()
        .build(schema)
        .map(|_| ())
        .map_err(|_| catalog_invalid(format!("capability {role} schema cannot be compiled")))
}

pub(crate) fn catalog_invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::CatalogIncompatible,
        DiagnosticCategory::Catalog,
        DiagnosticStage::Validate,
        message.into(),
    )
}
