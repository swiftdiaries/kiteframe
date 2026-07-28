#![forbid(unsafe_code)]

mod error;
mod ir;
mod validate;

use error::KiteframeDiagnosticError;
use ir::{PyResolvedAgent, PyResolvedCapabilityRequirement, PyResolvedSubagent};
use pyo3::prelude::*;

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyResolvedAgent>()?;
    module.add_class::<PyResolvedCapabilityRequirement>()?;
    module.add_class::<PyResolvedSubagent>()?;
    module.add(
        "KiteframeDiagnosticError",
        module.py().get_type::<KiteframeDiagnosticError>(),
    )?;
    module.add_function(wrap_pyfunction!(validate::load_resolved_agent, module)?)?;
    module.add_function(wrap_pyfunction!(validate::resolve_package, module)?)?;
    Ok(())
}
