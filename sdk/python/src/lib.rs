use pyo3::prelude::*;

mod client;
mod error;
mod types;

#[pymodule]
fn conproxy(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<client::ConproxyClient>()?;
    m.add_class::<types::PyQueryResponse>()?;
    m.add_class::<types::PySearchResult>()?;
    m.add_class::<types::PyBatchQueryResponse>()?;
    m.add_class::<types::PyFederatedQueryResponse>()?;
    m.add_class::<types::PyFederatedStats>()?;
    m.add_class::<types::PyStatsResponse>()?;
    m.add_class::<types::PyCircuitStatusResponse>()?;
    m.add_class::<types::PyQueueStatsResponse>()?;
    m.add_class::<types::PyClientInfo>()?;
    m.add_class::<types::PyClientsResponse>()?;
    m.add_class::<types::PyUpstreamInfo>()?;
    m.add_class::<types::PyPoolStatusResponse>()?;
    m.add_class::<types::PyReloadResponse>()?;
    m.add_class::<types::PyCacheClearResponse>()?;
    m.add_class::<types::PyCacheWarmupResponse>()?;
    m.add_class::<types::PyCacheEvictResponse>()?;
    m.add_class::<types::PyCacheIntegrityResponse>()?;
    m.add_class::<types::PyAgentInfo>()?;
    m.add_class::<types::PyListAgentsResponse>()?;
    m.add_class::<types::PyContextInfo>()?;
    m.add_class::<types::PyListContextsResponse>()?;
    m.add_class::<types::PySwitchContextResponse>()?;
    m.add_class::<types::PyCreateContextResponse>()?;
    m.add_class::<types::PyContextStats>()?;
    m.add_class::<types::PyDistillEntry>()?;
    m.add_class::<types::PySdkConfig>()?;
    Ok(())
}
