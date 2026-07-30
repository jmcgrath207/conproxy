use conproxy_sdk::SdkError;
use pyo3::exceptions::*;
use pyo3::PyErr;

pub fn to_py_err(err: SdkError) -> PyErr {
    match err {
        SdkError::Connection(msg) => PyConnectionError::new_err(msg),
        SdkError::Request { code, message } => {
            PyRuntimeError::new_err(format!("{}: {}", code, message))
        }
        SdkError::Config(msg) => PyValueError::new_err(msg),
        SdkError::Timeout => PyTimeoutError::new_err("Request timed out"),
    }
}
