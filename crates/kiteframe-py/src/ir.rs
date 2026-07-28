use std::sync::Arc;

use kiteframe_contract::{ResolvedAgent, ResolvedCapabilityRequirement, ResolvedSubagent};
use kiteframe_core::canonical_json;
use pyo3::{prelude::*, types::PyTuple};

use crate::error::diagnostic_error;

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

#[pymethods]
impl PyResolvedCapabilityRequirement {
    #[getter]
    fn name(&self) -> &str {
        self.inner.identity.name().as_str()
    }

    #[getter]
    fn version(&self) -> &str {
        self.inner.identity.version().as_str()
    }

    #[getter]
    fn required(&self) -> bool {
        self.inner.required
    }

    #[getter]
    fn resources<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.inner.resources)
    }
}

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

    fn canonical_json(&self) -> PyResult<Vec<u8>> {
        canonical_json(self.inner.as_ref()).map_err(|diagnostic| diagnostic_error(vec![diagnostic]))
    }
}
