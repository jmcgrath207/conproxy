use pyo3::prelude::*;
use std::sync::Arc;
use tokio::runtime::Runtime;

use conproxy_sdk::ConproxyClient as RustClient;
use conproxy_sdk::SdkConfig;

use crate::error::to_py_err;
use crate::types::*;

#[pyclass]
pub struct ConproxyClient {
    inner: Arc<RustClient>,
    rt: Arc<Runtime>,
}

#[pymethods]
impl ConproxyClient {
    #[new]
    #[pyo3(signature = (grpc_url=None, api_key=None))]
    fn new(grpc_url: Option<&str>, api_key: Option<&str>) -> PyResult<Self> {
        let config = match grpc_url {
            Some(url) => {
                let mut b = SdkConfig::builder().grpc_url(url);
                if let Some(key) = api_key {
                    b = b.api_key(key);
                }
                b.build()
            }
            None => SdkConfig::load().map_err(to_py_err)?,
        };
        let client = RustClient::new(config).map_err(to_py_err)?;
        let rt =
            Runtime::new().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(client),
            rt: Arc::new(rt),
        })
    }

    // === Search ===

    #[pyo3(signature = (q, top_k=None))]
    fn query(&self, q: &str, top_k: Option<u32>) -> PyResult<PyQueryResponse> {
        let resp = self
            .rt
            .block_on(self.inner.query(q, top_k.unwrap_or(10)))
            .map_err(to_py_err)?;
        Ok(PyQueryResponse::from(resp))
    }

    fn batch_query(&self, queries: Vec<String>) -> PyResult<PyBatchQueryResponse> {
        let refs: Vec<&str> = queries.iter().map(|s| s.as_str()).collect();
        let resp = self
            .rt
            .block_on(self.inner.batch_query(&refs))
            .map_err(to_py_err)?;
        Ok(PyBatchQueryResponse::from(resp))
    }

    #[pyo3(signature = (q, top_k=None))]
    fn federated_query(&self, q: &str, top_k: Option<u32>) -> PyResult<PyFederatedQueryResponse> {
        let resp = self
            .rt
            .block_on(self.inner.federated_query(q, vec![], top_k.unwrap_or(10)))
            .map_err(to_py_err)?;
        Ok(PyFederatedQueryResponse::from(resp))
    }

    // === Admin ===

    fn reload(&self) -> PyResult<PyReloadResponse> {
        let resp = self.rt.block_on(self.inner.reload()).map_err(to_py_err)?;
        Ok(PyReloadResponse::from(resp))
    }

    fn pause(&self) -> PyResult<()> {
        self.rt.block_on(self.inner.pause()).map_err(to_py_err)?;
        Ok(())
    }

    fn resume(&self) -> PyResult<()> {
        self.rt.block_on(self.inner.resume()).map_err(to_py_err)?;
        Ok(())
    }

    fn cache_clear(&self) -> PyResult<PyCacheClearResponse> {
        let resp = self
            .rt
            .block_on(self.inner.cache_clear())
            .map_err(to_py_err)?;
        Ok(PyCacheClearResponse::from(resp))
    }

    fn cache_warmup(&self, queries: Vec<String>) -> PyResult<PyCacheWarmupResponse> {
        let refs: Vec<&str> = queries.iter().map(|s| s.as_str()).collect();
        let resp = self
            .rt
            .block_on(self.inner.cache_warmup(&refs))
            .map_err(to_py_err)?;
        Ok(PyCacheWarmupResponse::from(resp))
    }

    #[pyo3(signature = (upstream_id, max_entries=100, expired_only=false))]
    fn cache_evict(
        &self,
        upstream_id: &str,
        max_entries: u32,
        expired_only: bool,
    ) -> PyResult<PyCacheEvictResponse> {
        let resp = self
            .rt
            .block_on(
                self.inner
                    .cache_evict(upstream_id, max_entries, expired_only),
            )
            .map_err(to_py_err)?;
        Ok(PyCacheEvictResponse::from(resp))
    }

    fn cache_integrity(&self) -> PyResult<PyCacheIntegrityResponse> {
        let resp = self
            .rt
            .block_on(self.inner.cache_integrity())
            .map_err(to_py_err)?;
        Ok(PyCacheIntegrityResponse::from(resp))
    }

    fn metrics_reset(&self) -> PyResult<()> {
        self.rt
            .block_on(self.inner.metrics_reset())
            .map_err(to_py_err)?;
        Ok(())
    }

    fn list_agents(&self) -> PyResult<PyListAgentsResponse> {
        let resp = self
            .rt
            .block_on(self.inner.list_agents())
            .map_err(to_py_err)?;
        Ok(PyListAgentsResponse::from(resp))
    }

    fn delete_agent(&self, agent_id: &str) -> PyResult<()> {
        self.rt
            .block_on(self.inner.delete_agent(agent_id))
            .map_err(to_py_err)?;
        Ok(())
    }

    fn rotate_key(&self, agent_id: &str, new_api_key: &str) -> PyResult<()> {
        self.rt
            .block_on(self.inner.rotate_key(agent_id, new_api_key))
            .map_err(to_py_err)?;
        Ok(())
    }

    // === Observability ===

    fn stats(&self) -> PyResult<PyStatsResponse> {
        let resp = self.rt.block_on(self.inner.stats()).map_err(to_py_err)?;
        Ok(PyStatsResponse::from(resp))
    }

    fn circuit_status(&self) -> PyResult<PyCircuitStatusResponse> {
        let resp = self
            .rt
            .block_on(self.inner.circuit_status())
            .map_err(to_py_err)?;
        Ok(PyCircuitStatusResponse::from(resp))
    }

    fn queue_stats(&self) -> PyResult<PyQueueStatsResponse> {
        let resp = self
            .rt
            .block_on(self.inner.queue_stats())
            .map_err(to_py_err)?;
        Ok(PyQueueStatsResponse::from(resp))
    }

    fn clients(&self) -> PyResult<PyClientsResponse> {
        let resp = self.rt.block_on(self.inner.clients()).map_err(to_py_err)?;
        Ok(PyClientsResponse::from(resp))
    }

    fn pool_status(&self) -> PyResult<PyPoolStatusResponse> {
        let resp = self
            .rt
            .block_on(self.inner.pool_status())
            .map_err(to_py_err)?;
        Ok(PyPoolStatusResponse::from(resp))
    }

    #[pyo3(signature = (context=None, tier=0, limit=0, include_stale=false))]
    fn distill(
        &self,
        context: Option<String>,
        tier: u32,
        limit: u32,
        include_stale: bool,
    ) -> PyResult<Vec<PyDistillEntry>> {
        let resp = self
            .rt
            .block_on(
                self.inner
                    .distill(context.unwrap_or_default(), tier, limit, include_stale),
            )
            .map_err(to_py_err)?;
        Ok(resp.into_iter().map(PyDistillEntry::from).collect())
    }

    // === Context ===

    fn list_contexts(&self) -> PyResult<PyListContextsResponse> {
        let resp = self
            .rt
            .block_on(self.inner.list_contexts())
            .map_err(to_py_err)?;
        Ok(PyListContextsResponse::from(resp))
    }

    fn current_context(&self) -> PyResult<PyContextInfo> {
        let resp = self
            .rt
            .block_on(self.inner.current_context())
            .map_err(to_py_err)?;
        Ok(PyContextInfo::from(resp))
    }

    fn switch_context(&self, context_id: &str) -> PyResult<PySwitchContextResponse> {
        let resp = self
            .rt
            .block_on(self.inner.switch_context(context_id))
            .map_err(to_py_err)?;
        Ok(PySwitchContextResponse::from(resp))
    }

    fn create_context(&self, context_id: &str) -> PyResult<PyCreateContextResponse> {
        let resp = self
            .rt
            .block_on(self.inner.create_context(context_id))
            .map_err(to_py_err)?;
        Ok(PyCreateContextResponse::from(resp))
    }

    fn context_stats(&self, context_id: &str) -> PyResult<PyContextStats> {
        let resp = self
            .rt
            .block_on(self.inner.context_stats(context_id))
            .map_err(to_py_err)?;
        Ok(PyContextStats::from(resp))
    }

    // === Async variants ===

    #[pyo3(signature = (q, top_k=None))]
    fn query_async<'py>(
        &self,
        py: Python<'py>,
        q: String,
        top_k: Option<u32>,
    ) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let client = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = client
                .query(&q, top_k.unwrap_or(10))
                .await
                .map_err(to_py_err)?;
            Ok(PyQueryResponse::from(resp))
        })
    }

    fn stats_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let client = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = client.stats().await.map_err(to_py_err)?;
            Ok(PyStatsResponse::from(resp))
        })
    }

    fn reload_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let client = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = client.reload().await.map_err(to_py_err)?;
            Ok(PyReloadResponse::from(resp))
        })
    }

    fn cache_clear_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let client = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = client.cache_clear().await.map_err(to_py_err)?;
            Ok(PyCacheClearResponse::from(resp))
        })
    }

    fn list_contexts_async<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        let client = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = client.list_contexts().await.map_err(to_py_err)?;
            Ok(PyListContextsResponse::from(resp))
        })
    }

    // === Context manager ===

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __exit__(
        &self,
        _exc_type: &Bound<'_, pyo3::types::PyAny>,
        _exc_val: &Bound<'_, pyo3::types::PyAny>,
        _exc_tb: &Bound<'_, pyo3::types::PyAny>,
    ) {
    }
}
