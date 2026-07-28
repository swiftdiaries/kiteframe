use std::sync::{Arc, LazyLock};

use kiteframe_contract::{
    CapabilityGrant as ContractCapabilityGrant, CapabilityGrantSet as ContractCapabilityGrantSet,
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    InvocationOutcome as ContractInvocationOutcome, InvocationStatus as ContractInvocationStatus,
};
use kiteframe_core::canonical_json;
use pyo3::{prelude::*, types::PyTuple};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

use crate::error::diagnostic_error;

static CAPABILITY_GRANT_SET_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/capability-grant-set.schema.json"
    ))
});
static INVOCATION_OUTCOME_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/invocation-outcome.schema.json"
    ))
});
static INVOCATION_STATUS_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/invocation-status.schema.json"
    ))
});

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "CapabilityGrant"
)]
pub struct PyCapabilityGrant {
    inner: Arc<ContractCapabilityGrant>,
}

impl From<ContractCapabilityGrant> for PyCapabilityGrant {
    fn from(inner: ContractCapabilityGrant) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCapabilityGrant {
    #[getter]
    pub fn name(&self) -> &str {
        self.inner.capability().name().as_str()
    }

    #[getter]
    pub fn version(&self) -> &str {
        self.inner.capability().version().as_str()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    pub fn resources<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .resources()
                .iter()
                .map(|resource| resource.as_str()),
        )
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "CapabilityGrantSet"
)]
pub struct PyCapabilityGrantSet {
    inner: Arc<ContractCapabilityGrantSet>,
}

impl From<ContractCapabilityGrantSet> for PyCapabilityGrantSet {
    fn from(inner: ContractCapabilityGrantSet) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCapabilityGrantSet {
    #[getter]
    pub fn admission_id(&self) -> &str {
        self.inner.admission_id().as_str()
    }

    #[getter]
    pub fn actor(&self) -> &str {
        self.inner.actor().as_str()
    }

    #[getter]
    pub fn agent(&self) -> &str {
        self.inner.agent().as_str()
    }

    #[getter]
    pub fn task(&self) -> &str {
        self.inner.task().as_str()
    }

    #[getter]
    pub fn session(&self) -> &str {
        self.inner.session().as_str()
    }

    #[getter]
    pub fn policy_revision(&self) -> &str {
        self.inner.policy_revision().as_str()
    }

    #[getter]
    pub fn catalog_digest(&self) -> String {
        self.inner.catalog_digest().to_string()
    }

    #[getter]
    pub fn issued_at(&self) -> u64 {
        self.inner.issued_at().unix_seconds()
    }

    #[getter]
    pub fn expires_at(&self) -> u64 {
        self.inner.expires_at().unix_seconds()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[CapabilityGrant, ...]",
        imports = ("builtins",)
    ))]
    pub fn grants<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let grants = self
            .inner
            .grants()
            .iter()
            .cloned()
            .map(PyCapabilityGrant::from)
            .map(|grant| Py::new(py, grant))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, grants)
    }

    #[getter]
    pub fn grant_digest(&self) -> String {
        self.inner.grant_digest().to_string()
    }

    #[gen_stub(override_return_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    pub fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "InvocationOutcome"
)]
pub struct PyInvocationOutcome {
    inner: Arc<ContractInvocationOutcome>,
}

impl From<ContractInvocationOutcome> for PyInvocationOutcome {
    fn from(inner: ContractInvocationOutcome) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyInvocationOutcome {
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "typing.Literal[\"succeeded\", \"failed\", \"denied\", \"suspended\", \"deferred\", \"outcome_unknown\"]",
        imports = ("typing",)
    ))]
    pub fn status(&self) -> &'static str {
        match self.inner.as_ref() {
            ContractInvocationOutcome::Succeeded { .. } => "succeeded",
            ContractInvocationOutcome::Failed { .. } => "failed",
            ContractInvocationOutcome::Denied { .. } => "denied",
            ContractInvocationOutcome::Suspended { .. } => "suspended",
            ContractInvocationOutcome::Deferred { .. } => "deferred",
            ContractInvocationOutcome::OutcomeUnknown { .. } => "outcome_unknown",
        }
    }

    #[getter]
    pub fn invocation_id(&self) -> &str {
        let invocation_id = match self.inner.as_ref() {
            ContractInvocationOutcome::Succeeded { invocation_id, .. }
            | ContractInvocationOutcome::Failed { invocation_id, .. }
            | ContractInvocationOutcome::Denied { invocation_id, .. }
            | ContractInvocationOutcome::Suspended { invocation_id, .. }
            | ContractInvocationOutcome::Deferred { invocation_id }
            | ContractInvocationOutcome::OutcomeUnknown { invocation_id, .. } => invocation_id,
        };
        invocation_id.as_str()
    }

    #[gen_stub(override_return_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    pub fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "InvocationStatus"
)]
pub struct PyInvocationStatus {
    inner: Arc<ContractInvocationStatus>,
}

impl From<ContractInvocationStatus> for PyInvocationStatus {
    fn from(inner: ContractInvocationStatus) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyInvocationStatus {
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "typing.Literal[\"pending\", \"suspended\", \"succeeded\", \"failed\", \"denied\", \"outcome_unknown\"]",
        imports = ("typing",)
    ))]
    pub fn status(&self) -> &'static str {
        match self.inner.as_ref() {
            ContractInvocationStatus::Pending { .. } => "pending",
            ContractInvocationStatus::Suspended { .. } => "suspended",
            ContractInvocationStatus::Succeeded { .. } => "succeeded",
            ContractInvocationStatus::Failed { .. } => "failed",
            ContractInvocationStatus::Denied { .. } => "denied",
            ContractInvocationStatus::OutcomeUnknown { .. } => "outcome_unknown",
        }
    }

    #[getter]
    pub fn invocation_id(&self) -> &str {
        let invocation_id = match self.inner.as_ref() {
            ContractInvocationStatus::Pending { invocation_id }
            | ContractInvocationStatus::Suspended { invocation_id, .. }
            | ContractInvocationStatus::Succeeded { invocation_id, .. }
            | ContractInvocationStatus::Failed { invocation_id, .. }
            | ContractInvocationStatus::Denied { invocation_id, .. }
            | ContractInvocationStatus::OutcomeUnknown { invocation_id, .. } => invocation_id,
        };
        invocation_id.as_str()
    }

    #[gen_stub(override_return_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    pub fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderResponseError {
    MalformedJson,
    LockedSchema,
    Contract,
}

pub fn load_capability_grant_set_inner(
    bytes: &[u8],
) -> Result<ContractCapabilityGrantSet, ProviderResponseError> {
    validate_locked_response(bytes, &CAPABILITY_GRANT_SET_SCHEMA)
}

pub fn load_invocation_outcome_inner(
    bytes: &[u8],
) -> Result<ContractInvocationOutcome, ProviderResponseError> {
    validate_locked_response(bytes, &INVOCATION_OUTCOME_SCHEMA)
}

pub fn load_invocation_status_inner(
    bytes: &[u8],
) -> Result<ContractInvocationStatus, ProviderResponseError> {
    validate_locked_response(bytes, &INVOCATION_STATUS_SCHEMA)
}

fn locked_response_validator(schema_bytes: &[u8]) -> jsonschema::Validator {
    let schema = serde_json::from_slice(schema_bytes)
        .expect("checked-in locked response schema must contain valid JSON");
    jsonschema::draft202012::options()
        .build(&schema)
        .expect("checked-in locked response schema must compile")
}

fn validate_locked_response<T>(
    bytes: &[u8],
    validator: &jsonschema::Validator,
) -> Result<T, ProviderResponseError>
where
    T: serde::de::DeserializeOwned,
{
    let response =
        serde_json::from_slice(bytes).map_err(|_| ProviderResponseError::MalformedJson)?;
    if !validator.is_valid(&response) {
        return Err(ProviderResponseError::LockedSchema);
    }
    serde_json::from_value(response).map_err(|_| ProviderResponseError::Contract)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_capability_grant_set(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyCapabilityGrantSet> {
    load_capability_grant_set_inner(bytes)
        .map(PyCapabilityGrantSet::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_invocation_outcome(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyInvocationOutcome> {
    load_invocation_outcome_inner(bytes)
        .map(PyInvocationOutcome::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_invocation_status(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyInvocationStatus> {
    load_invocation_status_inner(bytes)
        .map(PyInvocationStatus::from)
        .map_err(provider_response_error)
}

fn provider_response_error(_: ProviderResponseError) -> PyErr {
    diagnostic_error(vec![Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        "provider response is invalid",
    )])
}
