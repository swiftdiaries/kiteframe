use kiteframe_contract::Diagnostic;
use kiteframe_core::canonical_json;
use pyo3::{
    PyErr, Python,
    exceptions::{PyException, PyRuntimeError, PyValueError},
    types::{PyAnyMethods, PyBytes},
};
use pyo3_stub_gen::{
    TypeInfo,
    type_info::{MemberInfo, PyMethodsInfo},
};

pyo3_stub_gen::create_exception!(kiteframe._native, KiteframeDiagnosticError, PyException);

fn diagnostics_json_type() -> TypeInfo {
    TypeInfo::builtin("bytes")
}

fn code_type() -> TypeInfo {
    TypeInfo::builtin("str")
}

pyo3_stub_gen::inventory::submit! {
    PyMethodsInfo {
        struct_id: std::any::TypeId::of::<KiteframeDiagnosticError>,
        attrs: &[],
        getters: &[
            MemberInfo {
                name: "code",
                r#type: code_type,
                doc: "Stable code from the first canonical diagnostic.",
                default: None,
                deprecated: None,
            },
            MemberInfo {
                name: "diagnostics_json",
                r#type: diagnostics_json_type,
                doc: "Canonical, redacted diagnostic bytes.",
                default: None,
                deprecated: None,
            },
        ],
        setters: &[],
        methods: &[],
        file: file!(),
        line: line!(),
        column: column!(),
    }
}

pub(crate) fn diagnostic_error(mut diagnostics: Vec<Diagnostic>) -> PyErr {
    diagnostics.sort();
    let code = diagnostics
        .first()
        .map(|diagnostic| diagnostic.code.as_str())
        .unwrap_or("KF-RUNTIME-002");
    let message = diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.as_str())
        .unwrap_or("Kiteframe validation failed");
    let diagnostics_json = match canonical_json(&diagnostics) {
        Ok(bytes) => bytes,
        Err(_) => {
            return PyRuntimeError::new_err("Kiteframe diagnostics could not be serialized");
        }
    };

    Python::attach(|py| {
        let error = KiteframeDiagnosticError::new_err(message.to_owned());
        if error.value(py).setattr("code", code).is_err()
            || error
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
