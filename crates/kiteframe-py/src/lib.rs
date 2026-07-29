#![forbid(unsafe_code)]

mod error;
mod ir;
mod service;
mod validate;

use error::KiteframeDiagnosticError;
pub use ir::{PyResolvedAgent, PyResolvedCapabilityRequirement, PyResolvedSubagent};
use pyo3::prelude::*;
use pyo3_stub_gen::{StubGenConfig, StubInfo};
pub use service::{
    ProviderResponseError, PyAdmissionRequest, PyCapabilityCatalog, PyCapabilityGrant,
    PyCapabilityGrantSet, PyCatalogRequest, PyInvocationOutcome, PyInvocationRequest,
    PyInvocationStatus, load_admission_request_inner, load_capability_catalog_inner,
    load_capability_grant_set_for_request_inner, load_capability_grant_set_inner,
    load_catalog_request_inner, load_invocation_outcome_for_request_inner,
    load_invocation_outcome_inner, load_invocation_request_inner,
    load_invocation_status_for_invocation_id_inner, load_invocation_status_inner,
};

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyCatalogRequest>()?;
    module.add_class::<PyAdmissionRequest>()?;
    module.add_class::<PyInvocationRequest>()?;
    module.add_class::<PyCapabilityCatalog>()?;
    module.add_class::<PyCapabilityGrant>()?;
    module.add_class::<PyCapabilityGrantSet>()?;
    module.add_class::<PyInvocationOutcome>()?;
    module.add_class::<PyInvocationStatus>()?;
    module.add_class::<PyResolvedAgent>()?;
    module.add_class::<PyResolvedCapabilityRequirement>()?;
    module.add_class::<PyResolvedSubagent>()?;
    module.add(
        "KiteframeDiagnosticError",
        module.py().get_type::<KiteframeDiagnosticError>(),
    )?;
    module.add_function(wrap_pyfunction!(validate::load_resolved_agent, module)?)?;
    module.add_function(wrap_pyfunction!(validate::resolve_package, module)?)?;
    module.add_function(wrap_pyfunction!(service::load_catalog_request, module)?)?;
    module.add_function(wrap_pyfunction!(service::load_admission_request, module)?)?;
    module.add_function(wrap_pyfunction!(service::load_invocation_request, module)?)?;
    module.add_function(wrap_pyfunction!(service::load_capability_catalog, module)?)?;
    module.add_function(wrap_pyfunction!(
        service::load_capability_grant_set,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        service::load_capability_grant_set_for_request,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(service::load_invocation_outcome, module)?)?;
    module.add_function(wrap_pyfunction!(
        service::load_invocation_outcome_for_request,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(service::load_invocation_status, module)?)?;
    module.add_function(wrap_pyfunction!(
        service::load_invocation_status_for_invocation_id,
        module
    )?)?;
    Ok(())
}

pub fn python_stub() -> Result<String, String> {
    let stub = StubInfo::from_project_root(
        "kiteframe._native".to_owned(),
        std::path::PathBuf::new(),
        true,
        StubGenConfig::default(),
    )
    .map_err(|error| error.to_string())?;
    let module = stub
        .modules
        .get("kiteframe._native")
        .ok_or_else(|| "stub metadata for kiteframe._native is missing".to_owned())?;
    Ok(module.format_with_config(stub.config.use_type_statement))
}
