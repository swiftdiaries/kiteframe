use kiteframe_contract::Diagnostic;
use kiteframe_core::canonical_json;
use pyo3::{
    PyErr, Python,
    exceptions::{PyException, PyRuntimeError, PyValueError},
    types::{PyAnyMethods, PyBytes},
};

pyo3::create_exception!(_native, KiteframeDiagnosticError, PyException);

pub(crate) fn diagnostic_error(mut diagnostics: Vec<Diagnostic>) -> PyErr {
    diagnostics.sort();
    let diagnostics_json = match canonical_json(&diagnostics) {
        Ok(bytes) => bytes,
        Err(_) => {
            return PyRuntimeError::new_err("Kiteframe diagnostics could not be serialized");
        }
    };

    Python::attach(|py| {
        let error = KiteframeDiagnosticError::new_err("Kiteframe validation failed");
        if error
            .value(py)
            .setattr("diagnostics_json", PyBytes::new(py, &diagnostics_json))
            .is_err()
        {
            return PyRuntimeError::new_err("Kiteframe diagnostics could not be attached");
        }
        error
    })
}

pub(crate) fn ir_parse_error(_: serde_json::Error) -> PyErr {
    PyValueError::new_err("ResolvedAgent JSON is invalid")
}

pub(crate) fn canonical_ir_error() -> PyErr {
    PyValueError::new_err("ResolvedAgent JSON is not canonical")
}
