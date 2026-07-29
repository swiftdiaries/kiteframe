use std::{collections::BTreeMap, sync::Arc};

use kiteframe_contract::{
    BindingContentCapturePolicy, ComponentKind, ComponentMetadata, DataClassification,
    LatencyClass, ModelCapability, ModelLatencyClass, ModelModality, RegistrySymbol, ResolvedAgent,
    ResolvedCapabilityRequirement, ResolvedModelRequirement, ResolvedSubagent, RuntimeBinding,
    RuntimeTarget, ValidatedTextAsset,
};
use kiteframe_core::canonical_json;
use pyo3::{prelude::*, types::PyTuple};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::error::diagnostic_error;

fn component_kind_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Model => "model",
        ComponentKind::Middleware => "middleware",
        ComponentKind::Backend => "backend",
        ComponentKind::Checkpointer => "checkpointer",
        ComponentKind::CapabilityProvider => "capability_provider",
        ComponentKind::AuditSink => "audit_sink",
        ComponentKind::RedactionPolicy => "redaction_policy",
        ComponentKind::RetentionPolicy => "retention_policy",
        ComponentKind::AccessPolicy => "access_policy",
        ComponentKind::EncryptedContentStore => "encrypted_content_store",
    }
}

fn model_capability_name(capability: ModelCapability) -> &'static str {
    match capability {
        ModelCapability::Text => "text",
        ModelCapability::ToolCalling => "tool-calling",
        ModelCapability::StructuredOutput => "structured-output",
    }
}

fn latency_class_name(latency: ModelLatencyClass) -> &'static str {
    match latency {
        ModelLatencyClass::Interactive => "interactive",
        ModelLatencyClass::Batch => "batch",
    }
}

fn requirement_latency_class_name(latency: LatencyClass) -> &'static str {
    match latency {
        LatencyClass::Interactive => "interactive",
    }
}

fn model_modality_name(modality: ModelModality) -> &'static str {
    match modality {
        ModelModality::Text => "text",
    }
}

fn data_classification_name(classification: DataClassification) -> &'static str {
    match classification {
        DataClassification::Confidential => "confidential",
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedTextAsset"
)]
pub struct PyResolvedTextAsset {
    inner: Arc<ValidatedTextAsset>,
}

impl From<ValidatedTextAsset> for PyResolvedTextAsset {
    fn from(inner: ValidatedTextAsset) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedTextAsset {
    #[getter]
    fn path(&self) -> &str {
        self.inner.path.as_str()
    }

    #[getter]
    fn text(&self) -> &str {
        &self.inner.text
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedModelRequirement"
)]
pub struct PyResolvedModelRequirement {
    role: String,
    inner: Arc<ResolvedModelRequirement>,
}

impl PyResolvedModelRequirement {
    fn new(role: String, inner: ResolvedModelRequirement) -> Self {
        Self {
            role,
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedModelRequirement {
    #[getter]
    fn role(&self) -> &str {
        &self.role
    }

    #[getter]
    fn symbol(&self) -> &str {
        self.inner.symbol().as_str()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "builtins.tuple[builtins.str, ...]", imports = ("builtins",)))]
    fn capabilities<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .requirement()
                .capabilities
                .iter()
                .copied()
                .map(model_capability_name),
        )
    }

    #[getter]
    fn min_context_tokens(&self) -> Option<u32> {
        self.inner.requirement().min_context_tokens.map(Into::into)
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.str]", imports = ("typing", "builtins")))]
    fn max_latency_class(&self) -> Option<&'static str> {
        self.inner
            .requirement()
            .max_latency_class
            .map(requirement_latency_class_name)
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.str]", imports = ("typing", "builtins")))]
    fn residency(&self) -> Option<&str> {
        self.inner
            .requirement()
            .residency
            .as_ref()
            .map(|residency| residency.as_str())
    }

    #[getter]
    fn required(&self) -> bool {
        self.inner.requirement().required
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "RuntimeBinding"
)]
pub struct PyRuntimeBinding {
    inner: Arc<RuntimeBinding>,
}

impl From<RuntimeBinding> for PyRuntimeBinding {
    fn from(inner: RuntimeBinding) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "RuntimeBindingContentCapture"
)]
pub struct PyRuntimeBindingContentCapture {
    inner: Arc<BindingContentCapturePolicy>,
}

impl From<BindingContentCapturePolicy> for PyRuntimeBindingContentCapture {
    fn from(inner: BindingContentCapturePolicy) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyRuntimeBindingContentCapture {
    #[getter]
    fn enabled(&self) -> bool {
        self.inner.enabled
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "builtins.tuple[builtins.str, ...]", imports = ("builtins",)))]
    fn classifications<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .classifications
                .iter()
                .copied()
                .map(data_classification_name),
        )
    }

    #[getter]
    fn redaction_policy(&self) -> &str {
        self.inner.redaction_policy.as_str()
    }

    #[getter]
    fn retention_policy(&self) -> &str {
        self.inner.retention_policy.as_str()
    }

    #[getter]
    fn access_policy(&self) -> &str {
        self.inner.access_policy.as_str()
    }

    #[getter]
    fn encrypted_content_store(&self) -> &str {
        self.inner.encrypted_content_store.as_str()
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyRuntimeBinding {
    #[getter]
    fn runtime(&self) -> &str {
        self.inner.metadata.runtime.as_str()
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "builtins.tuple[builtins.tuple[builtins.str, builtins.str], ...]", imports = ("builtins",)))]
    fn model_symbols<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .spec
                .models
                .iter()
                .map(|(role, symbol)| (role.as_str(), symbol.as_str())),
        )
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "builtins.tuple[builtins.str, ...]", imports = ("builtins",)))]
    fn middleware_symbols<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .spec
                .components
                .middleware
                .iter()
                .map(RegistrySymbol::as_str),
        )
    }

    #[getter]
    fn backend(&self) -> Option<&str> {
        self.inner
            .spec
            .components
            .backend
            .as_ref()
            .map(RegistrySymbol::as_str)
    }

    #[getter]
    fn checkpointer(&self) -> Option<&str> {
        self.inner
            .spec
            .components
            .checkpointer
            .as_ref()
            .map(RegistrySymbol::as_str)
    }

    #[getter]
    fn capability_provider(&self) -> &str {
        self.inner.spec.capability_provider.as_str()
    }

    #[getter]
    fn audit_sink(&self) -> &str {
        self.inner.spec.audit_sink.as_str()
    }

    #[getter]
    fn content_capture(&self) -> Option<PyRuntimeBindingContentCapture> {
        self.inner
            .spec
            .content_capture
            .clone()
            .map(PyRuntimeBindingContentCapture::from)
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ComponentDescriptor"
)]
pub struct PyComponentDescriptor {
    symbol: String,
    inner: Arc<ComponentMetadata>,
}

impl PyComponentDescriptor {
    fn new(symbol: String, inner: ComponentMetadata) -> Self {
        Self {
            symbol,
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyComponentDescriptor {
    #[getter]
    fn symbol(&self) -> &str {
        &self.symbol
    }

    #[getter]
    fn kind(&self) -> &'static str {
        component_kind_name(self.inner.kind)
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "builtins.tuple[builtins.str, ...]", imports = ("builtins",)))]
    fn features<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner.features.iter().map(|feature| feature.as_str()),
        )
    }

    #[getter]
    fn durable(&self) -> bool {
        self.inner.durable
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.bool]", imports = ("typing", "builtins")))]
    fn model_tool_calling(&self) -> Option<bool> {
        self.inner.model.as_ref().map(|model| model.tool_calling)
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.bool]", imports = ("typing", "builtins")))]
    fn model_structured_output(&self) -> Option<bool> {
        self.inner
            .model
            .as_ref()
            .map(|model| model.structured_output)
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.int]", imports = ("typing", "builtins")))]
    fn model_max_context_tokens(&self) -> Option<u32> {
        self.inner
            .model
            .as_ref()
            .map(|model| model.max_context_tokens.into())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.str]", imports = ("typing", "builtins")))]
    fn model_residency(&self) -> Option<&str> {
        self.inner
            .model
            .as_ref()
            .map(|model| model.residency.as_str())
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.str]", imports = ("typing", "builtins")))]
    fn model_latency_class(&self) -> Option<&'static str> {
        self.inner
            .model
            .as_ref()
            .map(|model| latency_class_name(model.latency_class))
    }

    #[getter]
    #[gen_stub(override_return_type(type_repr = "typing.Optional[builtins.tuple[builtins.str, ...]]", imports = ("typing", "builtins")))]
    fn model_modalities<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyTuple>>> {
        self.inner
            .model
            .as_ref()
            .map(|model| {
                PyTuple::new(
                    py,
                    model.modalities.iter().copied().map(model_modality_name),
                )
            })
            .transpose()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedCapabilityRequirement"
)]
pub struct PyResolvedCapabilityRequirement {
    inner: Arc<ResolvedCapabilityRequirement>,
}

impl From<ResolvedCapabilityRequirement> for PyResolvedCapabilityRequirement {
    fn from(inner: ResolvedCapabilityRequirement) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedCapabilityRequirement {
    #[getter]
    fn name(&self) -> &str {
        self.inner.identity().name().as_str()
    }

    #[getter]
    fn version(&self) -> &str {
        self.inner.identity().version().as_str()
    }

    #[getter]
    fn required(&self) -> bool {
        self.inner.required()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    fn resources<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.inner.resources())
    }

    #[getter]
    fn descriptor_digest(&self) -> String {
        self.inner.descriptor_digest().to_string()
    }
    #[getter]
    fn input_schema_digest(&self) -> String {
        self.inner.input_schema_digest().to_string()
    }
    #[getter]
    fn output_schema_digest(&self) -> String {
        self.inner.output_schema_digest().to_string()
    }
    #[getter]
    fn stable_error_set_digest(&self) -> String {
        self.inner.stable_error_set_digest().to_string()
    }
    #[getter]
    fn safety_metadata_digest(&self) -> String {
        self.inner.safety_metadata_digest().to_string()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedSubagent"
)]
pub struct PyResolvedSubagent {
    inner: Arc<ResolvedSubagent>,
}

impl From<ResolvedSubagent> for PyResolvedSubagent {
    fn from(inner: ResolvedSubagent) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedSubagent {
    #[getter]
    fn package_name(&self) -> &str {
        self.inner.package_identity.name.as_str()
    }

    #[getter]
    fn package_version(&self) -> &str {
        self.inner.package_identity.version.as_str()
    }

    #[getter]
    fn resolved_digest(&self) -> String {
        self.inner.resolved_digest.to_string()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    fn delegated_capabilities<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .delegation
                .capabilities
                .iter()
                .map(|capability| capability.as_str()),
        )
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedAgent"
)]
pub struct PyResolvedAgent {
    inner: Arc<ResolvedAgent>,
}

impl From<ResolvedAgent> for PyResolvedAgent {
    fn from(inner: ResolvedAgent) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedAgent {
    #[getter]
    fn package_name(&self) -> &str {
        self.inner.package_identity().name.as_str()
    }

    #[getter]
    fn resolved_digest(&self) -> String {
        self.inner.resolved_digest().to_string()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ResolvedCapabilityRequirement, ...]",
        imports = ("builtins",)
    ))]
    fn capability_requirements<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .inner
            .capability_requirements()
            .iter()
            .cloned()
            .map(PyResolvedCapabilityRequirement::from)
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ResolvedTextAsset, ...]",
        imports = ("builtins",)
    ))]
    fn prompts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .inner
            .prompts()
            .values()
            .cloned()
            .map(PyResolvedTextAsset::from)
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ResolvedTextAsset, ...]",
        imports = ("builtins",)
    ))]
    fn skills<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .inner
            .skills()
            .values()
            .cloned()
            .map(PyResolvedTextAsset::from)
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ResolvedModelRequirement, ...]",
        imports = ("builtins",)
    ))]
    fn model_requirements<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .inner
            .models()
            .iter()
            .map(|(role, requirement)| {
                PyResolvedModelRequirement::new(role.as_str().to_owned(), requirement.clone())
            })
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    fn required_features<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .required_features()
                .iter()
                .map(|feature| feature.as_str()),
        )
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    fn optional_features<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .optional_features()
                .iter()
                .map(|feature| feature.as_str()),
        )
    }

    #[getter]
    fn content_capture_allowed(&self) -> bool {
        self.inner.content_capture().allowed
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[builtins.str, ...]",
        imports = ("builtins",)
    ))]
    fn content_capture_classifications<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner
                .content_capture()
                .classifications
                .iter()
                .map(|classification| match classification {
                    kiteframe_contract::DataClassification::Confidential => "confidential",
                }),
        )
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ResolvedSubagent, ...]",
        imports = ("builtins",)
    ))]
    fn subagents<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .inner
            .subagents()
            .iter()
            .cloned()
            .map(PyResolvedSubagent::from)
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }

    #[gen_stub(override_return_type(
        type_repr = "builtins.bytes",
        imports = ("builtins",)
    ))]
    fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }

    #[getter]
    fn portable_digest(&self) -> String {
        self.inner.portable_digest().to_string()
    }

    #[getter]
    fn lock_digest(&self) -> String {
        self.inner.lock_digest().to_string()
    }

    #[getter]
    fn catalog_name(&self) -> &str {
        &self.inner.catalog_identity().name
    }

    #[getter]
    fn catalog_revision(&self) -> &str {
        &self.inner.catalog_identity().revision
    }

    #[getter]
    fn catalog_digest(&self) -> String {
        self.inner.catalog_digest().to_string()
    }

    #[getter]
    fn binding_digest(&self) -> String {
        self.inner.binding_digest().to_string()
    }
}

#[gen_stub_pyclass]
#[pyclass(
    frozen,
    immutable_type,
    module = "kiteframe._native",
    name = "ResolvedRuntimeInputs"
)]
pub struct PyResolvedRuntimeInputs {
    resolved_agent: Arc<ResolvedAgent>,
    runtime_binding: Arc<RuntimeBinding>,
    runtime_target: Arc<RuntimeTarget>,
    target_components: Arc<BTreeMap<RegistrySymbol, ComponentMetadata>>,
}

impl PyResolvedRuntimeInputs {
    pub fn new(
        resolved_agent: ResolvedAgent,
        runtime_binding: RuntimeBinding,
        runtime_target: RuntimeTarget,
        target_components: BTreeMap<RegistrySymbol, ComponentMetadata>,
    ) -> Self {
        Self {
            resolved_agent: Arc::new(resolved_agent),
            runtime_binding: Arc::new(runtime_binding),
            runtime_target: Arc::new(runtime_target),
            target_components: Arc::new(target_components),
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyResolvedRuntimeInputs {
    #[getter]
    fn resolved_agent(&self) -> PyResolvedAgent {
        PyResolvedAgent {
            inner: Arc::clone(&self.resolved_agent),
        }
    }

    #[getter]
    fn runtime_binding(&self) -> PyRuntimeBinding {
        PyRuntimeBinding {
            inner: Arc::clone(&self.runtime_binding),
        }
    }

    #[getter]
    fn runtime_target(&self) -> &str {
        self.runtime_target.as_str()
    }

    #[getter]
    #[gen_stub(override_return_type(
        type_repr = "builtins.tuple[ComponentDescriptor, ...]",
        imports = ("builtins",)
    ))]
    fn target_components<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let values = self
            .target_components
            .iter()
            .map(|(symbol, metadata)| {
                PyComponentDescriptor::new(symbol.as_str().to_owned(), metadata.clone())
            })
            .map(|value| Py::new(py, value))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, values)
    }
}
