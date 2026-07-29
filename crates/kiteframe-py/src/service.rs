use std::sync::{Arc, LazyLock};

use kiteframe_contract::{
    AdmissionRequest as ContractAdmissionRequest, AuthorityRevision as ContractAuthorityRevision,
    AuthorityRevisionSet as ContractAuthorityRevisionSet,
    CapabilityCatalog as ContractCapabilityCatalog, CapabilityDenial as ContractCapabilityDenial,
    CapabilityDescriptor as ContractCapabilityDescriptor, CapabilityErrorDescriptor,
    CapabilityGrantSet as ContractCapabilityGrantSet, CatalogRequest as ContractCatalogRequest,
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity, DiagnosticStage,
    EffectClassification, EffectProposal as ContractEffectProposal,
    EffectiveCapabilityGrant as ContractEffectiveCapabilityGrant, EvidenceKind, ExecutionMode,
    InvocationOutcome as ContractInvocationOutcome, InvocationRequest as ContractInvocationRequest,
    InvocationStatus as ContractInvocationStatus, RetryClass, StableCapabilityError,
    StatusRequest as ContractStatusRequest, Suspension, TraceContext,
};
use kiteframe_core::canonical_json;
use pyo3::{
    IntoPyObjectExt,
    prelude::*,
    types::{PyDict, PyTuple},
};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

use crate::error::diagnostic_error;
use crate::ir::PyResolvedCapabilityRequirement;

static CATALOG_REQUEST_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/catalog-request.schema.json"
    ))
});
static ADMISSION_REQUEST_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/admission-request.schema.json"
    ))
});
static INVOCATION_REQUEST_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/invocation-request.schema.json"
    ))
});
static EFFECT_PROPOSAL_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/effect-proposal.schema.json"
    ))
});
static STATUS_REQUEST_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/status-request.schema.json"
    ))
});
static CAPABILITY_CATALOG_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    locked_response_validator(include_bytes!(
        "../../../schemas/v1alpha1/capability-catalog.schema.json"
    ))
});
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

pub(crate) fn json_value_to_python(
    py: Python<'_>,
    value: &serde_json::Value,
) -> PyResult<Py<PyAny>> {
    match value {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(value) => value.into_py_any(py),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                value.into_py_any(py)
            } else if let Some(value) = value.as_u64() {
                value.into_py_any(py)
            } else {
                value
                    .as_f64()
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("invalid JSON number"))?
                    .into_py_any(py)
            }
        }
        serde_json::Value::String(value) => value.into_py_any(py),
        serde_json::Value::Array(values) => {
            let values = values
                .iter()
                .map(|value| json_value_to_python(py, value))
                .collect::<PyResult<Vec<_>>>()?;
            Ok(PyTuple::new(py, values)?.into_any().unbind())
        }
        serde_json::Value::Object(values) => {
            let object = PyDict::new(py);
            for (key, value) in values {
                object.set_item(key, json_value_to_python(py, value)?)?;
            }
            Ok(object.into_any().unbind())
        }
    }
}

fn serialized_to_python<T: serde::Serialize + ?Sized>(
    py: Python<'_>,
    value: &T,
) -> PyResult<Py<PyAny>> {
    let value = serde_json::to_value(value)
        .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("value cannot be projected"))?;
    json_value_to_python(py, &value)
}

fn retry_name(retry: RetryClass) -> &'static str {
    match retry {
        RetryClass::Never => "never",
        RetryClass::AfterRefresh => "after_refresh",
        RetryClass::AfterUserAction => "after_user_action",
        RetryClass::StatusFirst => "status_first",
    }
}

fn execution_mode_name(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Immediate => "immediate",
        ExecutionMode::Deferred => "deferred",
        ExecutionMode::Suspendable => "suspendable",
    }
}

fn effect_name(effect: EffectClassification) -> &'static str {
    match effect {
        EffectClassification::ReadOnly => "read_only",
        EffectClassification::ReversibleWrite => "reversible_write",
        EffectClassification::IrreversibleWrite => "irreversible_write",
        EffectClassification::ExternalSideEffect => "external_side_effect",
    }
}

fn evidence_kind_name(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Confirmation => "confirmation",
        EvidenceKind::Approval => "approval",
        EvidenceKind::Consent => "consent",
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    skip_from_py_object,
    module = "kiteframe._native",
    name = "StableCapabilityError"
)]
#[derive(Clone)]
pub struct PyStableCapabilityError {
    inner: StableCapabilityError,
}

impl From<StableCapabilityError> for PyStableCapabilityError {
    fn from(inner: StableCapabilityError) -> Self {
        Self { inner }
    }
}

impl From<CapabilityErrorDescriptor> for PyStableCapabilityError {
    fn from(inner: CapabilityErrorDescriptor) -> Self {
        Self {
            inner: StableCapabilityError::try_new(
                inner.code(),
                inner.category(),
                inner.retry(),
                inner.message().clone(),
            )
            .expect("validated descriptor errors are valid stable capability errors"),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyStableCapabilityError {
    #[getter]
    pub fn code(&self) -> &str {
        self.inner.code()
    }
    #[getter]
    pub fn category(&self) -> &str {
        self.inner.category()
    }
    #[getter]
    pub fn retry(&self) -> &'static str {
        retry_name(self.inner.retry())
    }
    #[getter]
    pub fn message(&self) -> &str {
        self.inner.message().as_str()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    skip_from_py_object,
    module = "kiteframe._native",
    name = "Diagnostic"
)]
#[derive(Clone)]
pub struct PyDiagnostic {
    inner: Diagnostic,
}

impl From<Diagnostic> for PyDiagnostic {
    fn from(inner: Diagnostic) -> Self {
        Self { inner }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyDiagnostic {
    #[getter]
    pub fn code(&self) -> &'static str {
        self.inner.code.as_str()
    }
    #[getter]
    pub fn category(&self) -> PyResult<String> {
        serde_json::to_value(self.inner.category)
            .and_then(serde_json::from_value)
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("invalid diagnostic category"))
    }
    #[getter]
    pub fn severity(&self) -> &'static str {
        match self.inner.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
        }
    }
    #[getter]
    pub fn stage(&self) -> PyResult<String> {
        serde_json::to_value(self.inner.stage)
            .and_then(serde_json::from_value)
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("invalid diagnostic stage"))
    }
    #[getter]
    pub fn package_path(&self) -> Option<&str> {
        self.inner.package_path.as_deref()
    }
    #[getter]
    pub fn source_range(&self) -> Option<(u32, u32)> {
        self.inner
            .source_range
            .map(|range| (range.start, range.end))
    }
    #[getter]
    pub fn message(&self) -> &str {
        self.inner.message.as_str()
    }
    #[getter]
    pub fn help(&self) -> Option<&str> {
        self.inner
            .help
            .as_ref()
            .map(kiteframe_contract::SafeMessage::as_str)
    }
    #[getter]
    pub fn retry(&self) -> &'static str {
        retry_name(self.inner.retry)
    }
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.tuple[builtins.str, typing.Any], ...]",
        imports = ("builtins", "typing")
    ))]
    pub fn details<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let details = self
            .inner
            .details
            .iter()
            .map(|(key, value)| {
                PyTuple::new(py, [key.into_py_any(py)?, json_value_to_python(py, value)?])
            })
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, details)
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    skip_from_py_object,
    module = "kiteframe._native",
    name = "Suspension"
)]
#[derive(Clone)]
pub struct PySuspension {
    inner: Suspension,
}

impl From<Suspension> for PySuspension {
    fn from(inner: Suspension) -> Self {
        Self { inner }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PySuspension {
    #[getter]
    pub fn checkpoint_ref(&self) -> &str {
        self.inner.checkpoint_ref().as_str()
    }

    #[getter]
    pub fn evidence_kind(&self) -> &'static str {
        evidence_kind_name(self.inner.evidence_kind())
    }

    #[getter]
    pub fn evidence_request_ref(&self) -> &str {
        self.inner.evidence_request_ref().as_str()
    }

    #[getter]
    pub fn proposal_digest(&self) -> String {
        self.inner.proposal_digest().to_string()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "EffectProposal"
)]
pub struct PyEffectProposal {
    inner: Arc<ContractEffectProposal>,
}

impl From<ContractEffectProposal> for PyEffectProposal {
    fn from(inner: ContractEffectProposal) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyEffectProposal {
    #[getter]
    pub fn invocation_id(&self) -> &str {
        self.inner.invocation_id().as_str()
    }
    #[getter]
    pub fn admission_id(&self) -> &str {
        self.inner.admission_id().as_str()
    }
    #[getter]
    pub fn grant_digest(&self) -> String {
        self.inner.grant_digest().to_string()
    }
    #[getter]
    pub fn capability_name(&self) -> &str {
        self.inner.capability().name().as_str()
    }
    #[getter]
    pub fn capability_version(&self) -> &str {
        self.inner.capability().version().as_str()
    }
    #[getter]
    pub fn selected_resource(&self) -> &str {
        self.inner.selected_resource().as_str()
    }
    #[getter]
    pub fn arguments_digest(&self) -> String {
        self.inner.arguments_digest().to_string()
    }
    #[getter]
    pub fn preconditions_digest(&self) -> String {
        self.inner.preconditions_digest().to_string()
    }
    #[getter]
    pub fn idempotency_key(&self) -> Option<&str> {
        self.inner
            .idempotency_key()
            .map(kiteframe_contract::IdempotencyKey::as_str)
    }
    #[getter]
    pub fn effect(&self) -> &'static str {
        effect_name(self.inner.effect())
    }
    #[getter]
    pub fn proposal_digest(&self) -> String {
        self.inner.proposal_digest().to_string()
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
    name = "StatusRequest"
)]
pub struct PyStatusRequest {
    inner: Arc<ContractStatusRequest>,
}

impl From<ContractStatusRequest> for PyStatusRequest {
    fn from(inner: ContractStatusRequest) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyStatusRequest {
    #[getter]
    pub fn invocation_id(&self) -> &str {
        self.inner.invocation_id().as_str()
    }
    #[getter]
    pub fn traceparent(&self) -> &str {
        self.inner.trace_context().traceparent()
    }
    #[getter]
    pub fn tracestate(&self) -> Option<&str> {
        self.inner.trace_context().tracestate()
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn baggage(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.trace_context().baggage())
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
    name = "CapabilityDescriptor"
)]
pub struct PyCapabilityDescriptor {
    inner: Arc<ContractCapabilityDescriptor>,
}

impl From<ContractCapabilityDescriptor> for PyCapabilityDescriptor {
    fn from(inner: ContractCapabilityDescriptor) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCapabilityDescriptor {
    #[getter]
    pub fn name(&self) -> &str {
        self.inner.identity().name().as_str()
    }
    #[getter]
    pub fn version(&self) -> &str {
        self.inner.identity().version().as_str()
    }
    #[getter]
    pub fn summary(&self) -> &str {
        self.inner.summary()
    }
    #[getter]
    pub fn descriptor_digest(&self) -> String {
        self.inner.descriptor_digest().to_string()
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn input_schema(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value_to_python(py, self.inner.input_schema().as_value())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn output_schema(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value_to_python(py, self.inner.output_schema().as_value())
    }
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[StableCapabilityError, ...]",
        imports = ("builtins",)
    ))]
    pub fn stable_errors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let errors = self
            .inner
            .stable_errors()
            .iter()
            .cloned()
            .map(PyStableCapabilityError::from)
            .map(|error| Py::new(py, error))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, errors)
    }
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    pub fn execution_modes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .execution_modes()
                .as_set()
                .iter()
                .copied()
                .map(execution_mode_name),
        )
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn resource_selector_schema(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value_to_python(
            py,
            self.inner.resource_selector_schema().as_schema().as_value(),
        )
    }
    #[getter]
    pub fn effect(&self) -> PyResult<String> {
        serde_json::to_value(self.inner.effect())
            .and_then(serde_json::from_value)
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("invalid capability effect"))
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn idempotency(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.idempotency())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn freshness(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.freshness())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn preconditions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.preconditions())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn confirmation(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.confirmation())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn approval(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.approval())
    }
    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn consent(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.consent())
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "CatalogRequest"
)]
pub struct PyCatalogRequest {
    inner: Arc<ContractCatalogRequest>,
}

impl From<ContractCatalogRequest> for PyCatalogRequest {
    fn from(inner: ContractCatalogRequest) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCatalogRequest {
    #[staticmethod]
    #[pyo3(name = "default")]
    pub fn py_default(py: Python<'_>) -> PyResult<Self> {
        let secrets = py.import("secrets")?;
        let trace_id: String = secrets.call_method1("token_hex", (16,))?.extract()?;
        let parent_id: String = secrets.call_method1("token_hex", (8,))?.extract()?;
        let traceparent = format!("00-{trace_id}-{parent_id}-01");
        let trace_context = TraceContext::try_new(traceparent, None, Default::default())
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        Ok(ContractCatalogRequest::new(None, trace_context).into())
    }

    #[getter]
    pub fn known_catalog_digest(&self) -> Option<String> {
        self.inner.known_catalog_digest().map(ToString::to_string)
    }

    #[getter]
    pub fn traceparent(&self) -> &str {
        self.inner.trace_context().traceparent()
    }

    #[getter]
    pub fn tracestate(&self) -> Option<&str> {
        self.inner.trace_context().tracestate()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn baggage(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.trace_context().baggage())
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
    name = "AdmissionRequest"
)]
pub struct PyAdmissionRequest {
    inner: Arc<ContractAdmissionRequest>,
}

impl From<ContractAdmissionRequest> for PyAdmissionRequest {
    fn from(inner: ContractAdmissionRequest) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyAdmissionRequest {
    #[getter]
    pub fn catalog_name(&self) -> &str {
        &self.inner.catalog_identity().name
    }

    #[getter]
    pub fn catalog_revision(&self) -> &str {
        &self.inner.catalog_identity().revision
    }

    #[getter]
    pub fn catalog_digest(&self) -> String {
        self.inner.catalog_digest().to_string()
    }

    #[getter]
    pub fn request_digest(&self) -> String {
        self.inner.request_digest().to_string()
    }

    #[getter]
    pub fn traceparent(&self) -> &str {
        self.inner.trace_context().traceparent()
    }

    #[getter]
    pub fn tracestate(&self) -> Option<&str> {
        self.inner.trace_context().tracestate()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn baggage(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.trace_context().baggage())
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.tuple[builtins.str, builtins.str], ...]",
        imports = ("builtins",)
    ))]
    pub fn required_capabilities<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        capability_identities(py, self.inner.required_capabilities())
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.tuple[builtins.str, builtins.str], ...]",
        imports = ("builtins",)
    ))]
    pub fn optional_capabilities<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        capability_identities(py, self.inner.optional_capabilities())
    }

    #[gen_stub(override_return_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    pub fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }
}

fn capability_identities<'py>(
    py: Python<'py>,
    capabilities: &[kiteframe_contract::RequestedCapability],
) -> PyResult<Bound<'py, PyTuple>> {
    let identities = capabilities
        .iter()
        .map(|request| {
            PyTuple::new(
                py,
                [
                    request.capability().name().as_str(),
                    request.capability().version().as_str(),
                ],
            )
        })
        .collect::<PyResult<Vec<_>>>()?;
    PyTuple::new(py, identities)
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "InvocationRequest"
)]
pub struct PyInvocationRequest {
    inner: Arc<ContractInvocationRequest>,
}

impl From<ContractInvocationRequest> for PyInvocationRequest {
    fn from(inner: ContractInvocationRequest) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyInvocationRequest {
    #[getter]
    pub fn invocation_id(&self) -> &str {
        self.inner.invocation_id().as_str()
    }

    #[getter]
    pub fn admission_id(&self) -> &str {
        self.inner.admission_id().as_str()
    }

    #[getter]
    pub fn grant_digest(&self) -> String {
        self.inner.grant_digest().to_string()
    }

    #[getter]
    pub fn capability_name(&self) -> &str {
        self.inner.capability().name().as_str()
    }

    #[getter]
    pub fn capability_version(&self) -> &str {
        self.inner.capability().version().as_str()
    }

    #[getter]
    pub fn selected_resource(&self) -> &str {
        self.inner.selected_resource().as_str()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn arguments(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        json_value_to_python(py, self.inner.arguments())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn preconditions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.preconditions())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn evidence_refs(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.evidence_refs().as_map())
    }

    #[getter]
    pub fn idempotency_key(&self) -> Option<&str> {
        self.inner
            .idempotency_key()
            .map(kiteframe_contract::IdempotencyKey::as_str)
    }

    #[getter]
    pub fn traceparent(&self) -> &str {
        self.inner.trace_context().traceparent()
    }

    #[getter]
    pub fn tracestate(&self) -> Option<&str> {
        self.inner.trace_context().tracestate()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn baggage(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.trace_context().baggage())
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
    name = "CapabilityCatalog"
)]
pub struct PyCapabilityCatalog {
    inner: Arc<ContractCapabilityCatalog>,
}

impl From<ContractCapabilityCatalog> for PyCapabilityCatalog {
    fn from(inner: ContractCapabilityCatalog) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCapabilityCatalog {
    #[getter]
    pub fn name(&self) -> &str {
        &self.inner.identity().name
    }

    #[getter]
    pub fn revision(&self) -> &str {
        &self.inner.identity().revision
    }

    #[getter]
    pub fn catalog_digest(&self) -> String {
        self.inner.catalog_digest().to_string()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    pub fn descriptor_digests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .descriptors()
                .iter()
                .map(|descriptor| descriptor.descriptor_digest().to_string()),
        )
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
    name = "EffectiveCapabilityGrant"
)]
pub struct PyEffectiveCapabilityGrant {
    inner: Arc<ContractEffectiveCapabilityGrant>,
}

impl From<ContractEffectiveCapabilityGrant> for PyEffectiveCapabilityGrant {
    fn from(inner: ContractEffectiveCapabilityGrant) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyEffectiveCapabilityGrant {
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

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    pub fn execution_modes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .execution_modes()
                .as_set()
                .iter()
                .copied()
                .map(execution_mode_name),
        )
    }

    #[getter]
    pub fn maximum_effect(&self) -> PyResult<String> {
        serde_json::to_value(self.inner.maximum_effect())
            .and_then(serde_json::from_value)
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("invalid effect"))
    }

    #[getter]
    pub fn expires_at(&self) -> u64 {
        self.inner.expires_at().unix_seconds()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn required_evidence(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.required_evidence())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn freshness(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.freshness())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Any", imports = ("typing",)))]
    pub fn preconditions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        serialized_to_python(py, self.inner.preconditions())
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    skip_from_py_object,
    module = "kiteframe._native",
    name = "AuthorityRevision"
)]
#[derive(Clone)]
pub struct PyAuthorityRevision {
    inner: ContractAuthorityRevision,
}

impl From<ContractAuthorityRevision> for PyAuthorityRevision {
    fn from(inner: ContractAuthorityRevision) -> Self {
        Self { inner }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyAuthorityRevision {
    #[getter]
    pub fn source(&self) -> &str {
        self.inner.source()
    }

    #[getter]
    pub fn revision(&self) -> &str {
        self.inner.revision()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "AuthorityRevisionSet"
)]
pub struct PyAuthorityRevisionSet {
    inner: Arc<ContractAuthorityRevisionSet>,
}

impl From<ContractAuthorityRevisionSet> for PyAuthorityRevisionSet {
    fn from(inner: ContractAuthorityRevisionSet) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyAuthorityRevisionSet {
    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[AuthorityRevision, ...]",
        imports = ("builtins",)
    ))]
    pub fn entries<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let entries = self
            .inner
            .entries()
            .iter()
            .cloned()
            .map(PyAuthorityRevision::from)
            .map(|entry| Py::new(py, entry))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, entries)
    }

    #[getter]
    pub fn authority_revision_digest(&self) -> String {
        self.inner.authority_revision_digest().to_string()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    skip_from_py_object,
    module = "kiteframe._native",
    name = "CapabilityDenial"
)]
#[derive(Clone)]
pub struct PyCapabilityDenial {
    inner: ContractCapabilityDenial,
}

impl From<ContractCapabilityDenial> for PyCapabilityDenial {
    fn from(inner: ContractCapabilityDenial) -> Self {
        Self { inner }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyCapabilityDenial {
    #[getter]
    pub fn name(&self) -> &str {
        self.inner.capability().name().as_str()
    }
    #[getter]
    pub fn version(&self) -> &str {
        self.inner.capability().version().as_str()
    }
    #[getter]
    pub fn diagnostic(&self) -> PyDiagnostic {
        PyDiagnostic::from(self.inner.diagnostic().clone())
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
    pub fn admission_request_digest(&self) -> String {
        self.inner.admission_request_digest().to_string()
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
    pub fn catalog_name(&self) -> &str {
        &self.inner.catalog_identity().name
    }

    #[getter]
    pub fn catalog_revision(&self) -> &str {
        &self.inner.catalog_identity().revision
    }

    #[getter]
    pub fn catalog_digest(&self) -> String {
        self.inner.catalog_digest().to_string()
    }

    #[getter]
    pub fn authority_revisions(&self) -> PyAuthorityRevisionSet {
        PyAuthorityRevisionSet::from(self.inner.authority_revisions().clone())
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
        type_repr = "builtins.tuple[EffectiveCapabilityGrant, ...]",
        imports = ("builtins",)
    ))]
    pub fn grants<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let grants = self
            .inner
            .grants()
            .iter()
            .cloned()
            .map(PyEffectiveCapabilityGrant::from)
            .map(|grant| Py::new(py, grant))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, grants)
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[CapabilityDenial, ...]",
        imports = ("builtins",)
    ))]
    pub fn optional_denials<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let denials = self
            .inner
            .optional_denials()
            .iter()
            .cloned()
            .map(PyCapabilityDenial::from)
            .map(|denial| Py::new(py, denial))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, denials)
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

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "typing.Optional[typing.Any]",
        imports = ("typing",)
    ))]
    pub fn result(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.as_ref() {
            ContractInvocationOutcome::Succeeded { result, .. } => {
                json_value_to_python(py, result).map(Some)
            }
            _ => Ok(None),
        }
    }

    #[getter]
    pub fn error(&self) -> Option<PyStableCapabilityError> {
        match self.inner.as_ref() {
            ContractInvocationOutcome::Failed { error, .. } => {
                Some(PyStableCapabilityError::from(error.clone()))
            }
            _ => None,
        }
    }

    #[getter]
    pub fn diagnostic(&self) -> Option<PyDiagnostic> {
        self.inner.diagnostic().cloned().map(PyDiagnostic::from)
    }

    #[getter]
    pub fn suspension(&self) -> Option<PySuspension> {
        match self.inner.as_ref() {
            ContractInvocationOutcome::Suspended { suspension, .. } => {
                Some(PySuspension::from(suspension.clone()))
            }
            _ => None,
        }
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

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "typing.Optional[typing.Any]",
        imports = ("typing",)
    ))]
    pub fn result(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.as_ref() {
            ContractInvocationStatus::Succeeded { result, .. } => {
                json_value_to_python(py, result).map(Some)
            }
            _ => Ok(None),
        }
    }

    #[getter]
    pub fn error(&self) -> Option<PyStableCapabilityError> {
        match self.inner.as_ref() {
            ContractInvocationStatus::Failed { error, .. } => {
                Some(PyStableCapabilityError::from(error.clone()))
            }
            _ => None,
        }
    }

    #[getter]
    pub fn diagnostic(&self) -> Option<PyDiagnostic> {
        match self.inner.as_ref() {
            ContractInvocationStatus::Denied { diagnostic, .. } => {
                Some(PyDiagnostic::from(diagnostic.clone()))
            }
            ContractInvocationStatus::OutcomeUnknown { diagnostic, .. } => {
                Some(PyDiagnostic::from(diagnostic.diagnostic().clone()))
            }
            _ => None,
        }
    }

    #[getter]
    pub fn suspension(&self) -> Option<PySuspension> {
        match self.inner.as_ref() {
            ContractInvocationStatus::Suspended { suspension, .. } => {
                Some(PySuspension::from(suspension.clone()))
            }
            _ => None,
        }
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
    NonCanonical,
    LockedSchema,
    Contract,
    Correlation,
}

pub fn load_catalog_request_inner(
    bytes: &[u8],
) -> Result<ContractCatalogRequest, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &CATALOG_REQUEST_SCHEMA)
}

pub fn load_admission_request_inner(
    bytes: &[u8],
) -> Result<ContractAdmissionRequest, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &ADMISSION_REQUEST_SCHEMA)
}

pub fn load_invocation_request_inner(
    bytes: &[u8],
) -> Result<ContractInvocationRequest, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &INVOCATION_REQUEST_SCHEMA)
}

pub fn load_effect_proposal_inner(
    bytes: &[u8],
) -> Result<ContractEffectProposal, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &EFFECT_PROPOSAL_SCHEMA)
}

pub fn load_status_request_inner(
    bytes: &[u8],
) -> Result<ContractStatusRequest, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &STATUS_REQUEST_SCHEMA)
}

pub fn load_capability_catalog_inner(
    bytes: &[u8],
) -> Result<ContractCapabilityCatalog, ProviderResponseError> {
    validate_canonical_locked_response(bytes, &CAPABILITY_CATALOG_SCHEMA)
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

pub fn load_capability_grant_set_for_request_inner(
    bytes: &[u8],
    request: &ContractAdmissionRequest,
) -> Result<ContractCapabilityGrantSet, ProviderResponseError> {
    let response = load_capability_grant_set_inner(bytes)?;
    response
        .validate_against(request)
        .map_err(|_| ProviderResponseError::Correlation)?;
    Ok(response)
}

pub fn load_invocation_outcome_for_request_inner(
    bytes: &[u8],
    request: &ContractInvocationRequest,
    descriptor: &ContractCapabilityDescriptor,
) -> Result<ContractInvocationOutcome, ProviderResponseError> {
    let response = load_invocation_outcome_inner(bytes)?;
    response
        .validate_against(request, descriptor)
        .map_err(|_| ProviderResponseError::Correlation)?;
    Ok(response)
}

pub fn load_invocation_status_for_request_inner(
    bytes: &[u8],
    request: &ContractStatusRequest,
    descriptor: &ContractCapabilityDescriptor,
) -> Result<ContractInvocationStatus, ProviderResponseError> {
    let response = load_invocation_status_inner(bytes)?;
    response
        .validate_for_status_request(request, descriptor)
        .map_err(|_| ProviderResponseError::Correlation)?;
    Ok(response)
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
    validate_locked_value(response, validator)
}

fn validate_canonical_locked_response<T>(
    bytes: &[u8],
    validator: &jsonschema::Validator,
) -> Result<T, ProviderResponseError>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let response =
        serde_json::from_slice(bytes).map_err(|_| ProviderResponseError::MalformedJson)?;
    let typed = validate_locked_value(response, validator)?;
    if canonical_json(&typed).map_err(|_| ProviderResponseError::Contract)? != bytes {
        return Err(ProviderResponseError::NonCanonical);
    }
    Ok(typed)
}

fn validate_locked_value<T>(
    response: serde_json::Value,
    validator: &jsonschema::Validator,
) -> Result<T, ProviderResponseError>
where
    T: serde::de::DeserializeOwned,
{
    if !validator.is_valid(&response) {
        return Err(ProviderResponseError::LockedSchema);
    }
    serde_json::from_value(response).map_err(|_| ProviderResponseError::Contract)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_catalog_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyCatalogRequest> {
    load_catalog_request_inner(bytes)
        .map(PyCatalogRequest::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_admission_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyAdmissionRequest> {
    load_admission_request_inner(bytes)
        .map(PyAdmissionRequest::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_invocation_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyInvocationRequest> {
    load_invocation_request_inner(bytes)
        .map(PyInvocationRequest::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_effect_proposal(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyEffectProposal> {
    load_effect_proposal_inner(bytes)
        .map(PyEffectProposal::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_status_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyStatusRequest> {
    load_status_request_inner(bytes)
        .map(PyStatusRequest::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_capability_catalog(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
) -> PyResult<PyCapabilityCatalog> {
    load_capability_catalog_inner(bytes)
        .map(PyCapabilityCatalog::from)
        .map_err(provider_response_error)
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

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_capability_grant_set_for_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
    request: &PyAdmissionRequest,
) -> PyResult<PyCapabilityGrantSet> {
    load_capability_grant_set_for_request_inner(bytes, request.inner.as_ref())
        .map(PyCapabilityGrantSet::from)
        .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_invocation_outcome_for_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
    request: &PyInvocationRequest,
    requirement: &PyResolvedCapabilityRequirement,
) -> PyResult<PyInvocationOutcome> {
    load_invocation_outcome_for_request_inner(
        bytes,
        request.inner.as_ref(),
        requirement.descriptor_inner(),
    )
    .map(PyInvocationOutcome::from)
    .map_err(provider_response_error)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn load_invocation_status_for_request(
    #[gen_stub(override_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    bytes: &[u8],
    request: &PyStatusRequest,
    requirement: &PyResolvedCapabilityRequirement,
) -> PyResult<PyInvocationStatus> {
    load_invocation_status_for_request_inner(
        bytes,
        request.inner.as_ref(),
        requirement.descriptor_inner(),
    )
    .map(PyInvocationStatus::from)
    .map_err(provider_response_error)
}

fn provider_response_error(error: ProviderResponseError) -> PyErr {
    let message = match error {
        ProviderResponseError::NonCanonical => "provider payload is not canonical",
        ProviderResponseError::MalformedJson
        | ProviderResponseError::LockedSchema
        | ProviderResponseError::Contract
        | ProviderResponseError::Correlation => "provider response is invalid",
    };
    diagnostic_error(vec![Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message,
    )])
}
