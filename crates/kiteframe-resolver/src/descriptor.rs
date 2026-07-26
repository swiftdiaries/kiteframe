use jsonschema::draft202012;
use kiteframe_contract::{
    CapabilityDescriptor, CapabilityIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, Sha256Digest,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// A descriptor plus independently verifiable canonical digests for lock construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedDescriptor {
    descriptor: CapabilityDescriptor,
    input_schema_digest: Sha256Digest,
    output_schema_digest: Sha256Digest,
    stable_error_set_digest: Sha256Digest,
    safety_metadata_digest: Sha256Digest,
}

impl ValidatedDescriptor {
    pub fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }

    pub fn identity(&self) -> &CapabilityIdentity {
        self.descriptor.identity()
    }

    pub fn input_schema_digest(&self) -> &Sha256Digest {
        &self.input_schema_digest
    }

    pub fn output_schema_digest(&self) -> &Sha256Digest {
        &self.output_schema_digest
    }

    pub fn stable_error_set_digest(&self) -> &Sha256Digest {
        &self.stable_error_set_digest
    }

    pub fn safety_metadata_digest(&self) -> &Sha256Digest {
        &self.safety_metadata_digest
    }
}

pub(crate) fn validate_descriptor(
    descriptor: CapabilityDescriptor,
) -> Result<ValidatedDescriptor, Diagnostic> {
    validate_schema(descriptor.input_schema().as_value(), "input")?;
    validate_schema(descriptor.output_schema().as_value(), "output")?;

    let wire = serde_json::to_value(&descriptor)
        .map_err(|_| catalog_invalid("capability descriptor cannot be serialized canonically"))?;
    let object = wire
        .as_object()
        .ok_or_else(|| catalog_invalid("capability descriptor must serialize as an object"))?;

    Ok(ValidatedDescriptor {
        input_schema_digest: digest_part("input-schema", field(object, "inputSchema")?)?,
        output_schema_digest: digest_part("output-schema", field(object, "outputSchema")?)?,
        stable_error_set_digest: digest_part("stable-errors", field(object, "stableErrors")?)?,
        safety_metadata_digest: digest_part("safety-metadata", &safety_metadata(object)?)?,
        descriptor,
    })
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

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, Diagnostic> {
    object
        .get(name)
        .ok_or_else(|| catalog_invalid(format!("capability descriptor omits {name}")))
}

fn safety_metadata(object: &Map<String, Value>) -> Result<Value, Diagnostic> {
    let mut safety = Map::new();
    for name in [
        "executionModes",
        "resourceSelectorSchema",
        "effect",
        "idempotency",
        "freshness",
        "preconditions",
        "confirmation",
        "approval",
        "consent",
    ] {
        safety.insert(name.to_owned(), field(object, name)?.clone());
    }
    Ok(Value::Object(safety))
}

fn digest_part(domain: &str, value: &Value) -> Result<Sha256Digest, Diagnostic> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| catalog_invalid("capability descriptor material cannot be canonicalized"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe.dev/capability-descriptor/");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}
