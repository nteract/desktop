//! Error types for Python bindings.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

pyo3::create_exception!(runtimed, RuntimedError, PyException);

/// Convert runtimed errors to Python exceptions.
pub fn to_py_err(err: impl std::fmt::Display) -> PyErr {
    RuntimedError::new_err(err.to_string())
}

/// Emit a DeprecationWarning via Python's warnings module.
pub fn emit_deprecation_warning(py: Python<'_>, message: &str) -> PyResult<()> {
    let warnings = py.import("warnings")?;
    warnings.call_method1(
        "warn",
        (
            message,
            py.get_type::<pyo3::exceptions::PyDeprecationWarning>(),
        ),
    )?;
    Ok(())
}
