use pyo3::prelude::*;
use pyo3::types::PyAny;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

use conproxy::{Engine as RustEngine, EngineStats, QueryOpts};

use crate::types::PyQueryResponse;

enum MaxMem {
    Bytes(u64),
    Str(String),
}

struct BuildArgs {
    config: Option<String>,
    persist_path: Option<String>,
    ttl: Option<u64>,
    stale_ttl: Option<u64>,
    context: Option<String>,
    semantic: Option<bool>,
    threshold: Option<f32>,
    max_entries: Option<usize>,
    max_memory: Option<MaxMem>,
    memory_fraction: Option<f64>,
    dashboard_listen: Option<String>,
}

fn build_rust(args: BuildArgs) -> PyResult<RustEngine> {
    let mut b = RustEngine::builder();
    if let Some(c) = args.config {
        b = b.config_toml(c);
    }
    if let Some(p) = args.persist_path {
        b = b.persist_path(p);
    }
    if let Some(t) = args.ttl {
        b = b.ttl(Duration::from_secs(t));
    }
    if let Some(t) = args.stale_ttl {
        b = b.stale_ttl(Duration::from_secs(t));
    }
    if let Some(c) = args.context {
        b = b.context(c);
    }
    if let Some(s) = args.semantic {
        b = b.semantic(s);
    }
    b = b.threshold(args.threshold);
    if let Some(n) = args.max_entries {
        b = b.max_entries(n);
    }
    if let Some(mm) = args.max_memory {
        b = match mm {
            MaxMem::Bytes(n) => b.max_memory(Some(n)),
            MaxMem::Str(s) => b
                .max_memory_parsed(&s)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
        };
    }
    if let Some(f) = args.memory_fraction {
        b = b.memory_fraction(f);
    }
    if let Some(d) = args.dashboard_listen {
        b = b.dashboard_listen(d);
    }
    b.build()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

fn extract_max_memory(max_memory: Option<&Bound<'_, PyAny>>) -> PyResult<Option<MaxMem>> {
    match max_memory {
        None => Ok(None),
        Some(v) if v.extract::<u64>().is_ok() => Ok(Some(MaxMem::Bytes(v.extract()?))),
        Some(v) => Ok(Some(MaxMem::Str(v.extract()?))),
    }
}

fn wrap_engine(inner: RustEngine) -> PyResult<PyEngine> {
    let rt =
        Runtime::new().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    rt.block_on(async {
        inner.spawn_refresh();
        inner.spawn_memory_ticker();
        inner.spawn_dashboard();
    });
    Ok(PyEngine {
        inner: Arc::new(inner),
        rt: Arc::new(rt),
    })
}

/// In-process query-core cache. Always `await query()`.
#[pyclass(name = "Engine")]
pub struct PyEngine {
    inner: Arc<RustEngine>,
    rt: Arc<Runtime>,
}

#[pymethods]
impl PyEngine {
    #[new]
    #[pyo3(signature = (config=None, persist_path=None, ttl=None, stale_ttl=None, context=None, semantic=None, threshold=None, max_entries=None, max_memory=None, memory_fraction=None, dashboard_listen=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: Option<String>,
        persist_path: Option<String>,
        ttl: Option<u64>,
        stale_ttl: Option<u64>,
        context: Option<String>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        max_entries: Option<usize>,
        max_memory: Option<&Bound<'_, PyAny>>,
        memory_fraction: Option<f64>,
        dashboard_listen: Option<String>,
    ) -> PyResult<Self> {
        let inner = build_rust(BuildArgs {
            config,
            persist_path,
            ttl,
            stale_ttl,
            context,
            semantic,
            threshold,
            max_entries,
            max_memory: extract_max_memory(max_memory)?,
            memory_fraction,
            dashboard_listen,
        })?;
        wrap_engine(inner)
    }

    #[classmethod]
    #[pyo3(signature = (config=None, persist_path=None, ttl=None, stale_ttl=None, context=None, semantic=None, threshold=None, max_entries=None, max_memory=None, memory_fraction=None, dashboard_listen=None))]
    #[allow(clippy::too_many_arguments)]
    fn create<'py>(
        _cls: &Bound<'py, pyo3::types::PyType>,
        py: Python<'py>,
        config: Option<String>,
        persist_path: Option<String>,
        ttl: Option<u64>,
        stale_ttl: Option<u64>,
        context: Option<String>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        max_entries: Option<usize>,
        max_memory: Option<&Bound<'py, PyAny>>,
        memory_fraction: Option<f64>,
        dashboard_listen: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = BuildArgs {
            config,
            persist_path,
            ttl,
            stale_ttl,
            context,
            semantic,
            threshold,
            max_entries,
            max_memory: extract_max_memory(max_memory)?,
            memory_fraction,
            dashboard_listen,
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Build and wrap off the async runtime: `wrap_engine` owns a
            // fresh tokio `Runtime` and `block_on`s the spawn calls, which
            // would panic if run inside the bridge runtime's worker threads.
            let wrapped = tokio::task::spawn_blocking(move || {
                let inner = build_rust(args)?;
                wrap_engine(inner)
            })
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))??;
            Python::attach(|py| Bound::new(py, wrapped).map(|b| b.into_any().unbind()))
        })
    }

    #[pyo3(signature = (q, top_k=None, context=None, ttl=None, semantic=None, threshold=None, skip_cache=false))]
    #[allow(clippy::too_many_arguments)]
    fn query<'py>(
        slf: Bound<'py, Self>,
        py: Python<'py>,
        q: String,
        top_k: Option<usize>,
        context: Option<String>,
        ttl: Option<u64>,
        semantic: Option<bool>,
        threshold: Option<f32>,
        skip_cache: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = slf.borrow().inner.clone();
        let rt = slf.borrow().rt.clone();
        let opts = QueryOpts {
            top_k,
            context,
            ttl: ttl.map(Duration::from_secs),
            semantic,
            threshold,
            skip_cache,
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let resp = tokio::task::spawn_blocking(move || {
                rt.block_on(async { engine.query(&q, opts).await })
            })
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Python::attach(|py| {
                Bound::new(py, PyQueryResponse::from_engine(resp)).map(|b| b.into_any().unbind())
            })
        })
    }

    fn clear(&self) {
        self.inner.clear();
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let s: EngineStats = self.inner.stats();
        let d = pyo3::types::PyDict::new(py);
        d.set_item("hits", s.hits)?;
        d.set_item("misses", s.misses)?;
        d.set_item("size", s.size)?;
        d.set_item("evictions", s.evictions)?;
        Ok(d)
    }

    fn close(&self) {
        self.inner.close();
    }

    /// Bound dashboard address (`"127.0.0.1:PORT"`), or `None` when no
    /// `dashboard_listen` was given.
    fn dashboard_addr(&self) -> Option<String> {
        self.inner.dashboard_addr().map(|a| a.to_string())
    }

    fn __aenter__<'py>(slf: Py<Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(slf) })
    }

    fn __aexit__<'py>(
        slf: Bound<'py, Self>,
        py: Python<'py>,
        _exc_type: Option<Bound<'py, PyAny>>,
        _exc: Option<Bound<'py, PyAny>>,
        _tb: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        slf.borrow().close();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(false) })
    }
}
