use std::sync::Arc;

use kiteframe_contract::{ResolvedAgent, ResolvedCapabilityRequirement, ResolvedSubagent};
use kiteframe_core::canonical_json;
use pyo3::{prelude::*, types::PyTuple};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::error::diagnostic_error;

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
