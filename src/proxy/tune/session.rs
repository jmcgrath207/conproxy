//! Process-local tune session store (plan 09).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Default idle TTL for tune sessions (1 hour).
pub const DEFAULT_SESSION_TTL_SECS: u64 = 3600;

/// One recorded tool run inside a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneRunRecord {
    pub run_id: String,
    pub tool: String,
    pub params: serde_json::Value,
    pub metrics: serde_json::Value,
    /// When true, export prefers this run.
    #[serde(default)]
    pub selected: bool,
}

/// Live tune session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneSession {
    pub session_id: String,
    pub agent_id: String,
    pub context_id: String,
    pub created_at_ms: u64,
    #[serde(skip, default = "instant_now")]
    pub last_touch: Instant,
    pub runs: Vec<TuneRunRecord>,
}

fn instant_now() -> Instant {
    Instant::now()
}

impl TuneSession {
    fn touch(&mut self) {
        self.last_touch = Instant::now();
    }
}

/// Summary for list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneSessionSummary {
    pub session_id: String,
    pub agent_id: String,
    pub context_id: String,
    pub run_count: usize,
    pub created_at_ms: u64,
}

/// Export formats for happy-path paste into config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneExportFormats {
    pub toml: String,
    pub json: serde_json::Value,
}

/// Full export artifact (contexts.<id> only — no global proxy.*).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuneExportArtifact {
    pub session_id: String,
    pub agent_id: String,
    pub context_id: String,
    pub scope: serde_json::Value,
    #[serde(default)]
    pub other: serde_json::Value,
    pub formats: TuneExportFormats,
    #[serde(default)]
    pub source_run_id: Option<String>,
}

struct Inner {
    sessions: HashMap<String, TuneSession>,
    ttl: Duration,
    next_id: AtomicU64,
}

/// Distinguishes why `get`/`get_checked` could not return a session.
/// Used to surface clearer errors to MCP clients (was a single
/// `"session not found"` for every reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// `session_id` was not found in the store.
    Unknown { session_id: String },
    /// Session exists but `agent_id` filter did not match.
    AgentMismatch {
        session_id: String,
        expected: String,
        got: String,
    },
    /// Session exists, agent matched, but `context_id` did not.
    ContextMismatch {
        session_id: String,
        expected: String,
        got: String,
    },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { session_id } => {
                write!(f, "unknown session_id: {session_id}")
            }
            Self::AgentMismatch {
                session_id,
                expected,
                got,
            } => write!(
                f,
                "agent_id mismatch for session {session_id}: session is owned by {expected:?}, got {got:?}"
            ),
            Self::ContextMismatch {
                session_id,
                expected,
                got,
            } => write!(
                f,
                "context_id mismatch for session {session_id}: session is for {expected:?}, got {got:?}"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

/// Thread-safe session map with idle TTL eviction on access.
#[derive(Clone)]
pub struct TuneSessionStore {
    inner: Arc<Mutex<Inner>>,
}

fn new_id(prefix: &str, counter: &AtomicU64) -> String {
    let n = counter.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos:x}-{n}")
}

impl TuneSessionStore {
    /// Create a store with the given idle TTL (seconds).
    #[must_use]
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                sessions: HashMap::new(),
                ttl: Duration::from_secs(ttl_secs.max(1)),
                next_id: AtomicU64::new(1),
            })),
        }
    }

    /// Open a new session (or reuse `session_id` if provided and owned).
    ///
    /// # Errors
    ///
    /// Returns an error when `session_id` is set but belongs to another
    /// agent/context or is unknown.
    pub fn open(
        &self,
        agent_id: String,
        context_id: String,
        session_id: Option<String>,
    ) -> Result<TuneSession, String> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);

        if let Some(ref id) = session_id {
            if let Some(existing) = g.sessions.get_mut(id) {
                if existing.agent_id != agent_id || existing.context_id != context_id {
                    let detail = if existing.agent_id != agent_id {
                        format!(
                            "agent_id mismatch for session {id}: session is owned by {:?}, got {:?}",
                            existing.agent_id, agent_id
                        )
                    } else {
                        format!(
                            "context_id mismatch for session {id}: session is for {:?}, got {:?}",
                            existing.context_id, context_id
                        )
                    };
                    return Err(detail);
                }
                existing.touch();
                return Ok(existing.clone());
            }
            return Err(format!("unknown session_id: {id}"));
        }

        let id = new_id("sess", &g.next_id);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let sess = TuneSession {
            session_id: id.clone(),
            agent_id,
            context_id,
            created_at_ms: now_ms,
            last_touch: Instant::now(),
            runs: Vec::new(),
        };
        g.sessions.insert(id, sess.clone());
        Ok(sess)
    }

    /// Get session if caller matches agent/context filters.
    pub fn get(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        context_id: Option<&str>,
    ) -> Option<TuneSession> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);
        let s = g.sessions.get_mut(session_id)?;
        if let Some(a) = agent_id {
            if s.agent_id != a {
                return None;
            }
        }
        if let Some(c) = context_id {
            if s.context_id != c {
                return None;
            }
        }
        s.touch();
        Some(s.clone())
    }

    /// Get session with explicit error reason (preferred over [`Self::get`]
    /// when the caller needs to surface a useful error to an MCP client).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Unknown`], [`SessionError::AgentMismatch`], or
    /// [`SessionError::ContextMismatch`] as appropriate.
    pub fn get_checked(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        context_id: Option<&str>,
    ) -> Result<TuneSession, SessionError> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);
        let s = g
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::Unknown {
                session_id: session_id.to_string(),
            })?;
        if let Some(a) = agent_id {
            if s.agent_id != a {
                return Err(SessionError::AgentMismatch {
                    session_id: session_id.to_string(),
                    expected: s.agent_id.clone(),
                    got: a.to_string(),
                });
            }
        }
        if let Some(c) = context_id {
            if s.context_id != c {
                return Err(SessionError::ContextMismatch {
                    session_id: session_id.to_string(),
                    expected: s.context_id.clone(),
                    got: c.to_string(),
                });
            }
        }
        s.touch();
        Ok(s.clone())
    }

    /// Close session; returns true if removed.
    ///
    /// The bool is the simple “was a session removed” signal. Prefer
    /// [`Self::close_with_reason`] when the MCP caller benefits from knowing
    /// whether the failure was “unknown id” or “agent mismatch”.
    pub fn close(&self, session_id: &str, agent_id: Option<&str>) -> bool {
        self.close_with_reason(session_id, agent_id).is_ok()
    }

    /// Like [`Self::close`] but reports the precise reason on failure.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Unknown`] if the session id does not exist,
    /// or [`SessionError::AgentMismatch`] if the `agent_id` filter did not
    /// match the session owner.
    pub fn close_with_reason(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get(session_id)
            .ok_or_else(|| SessionError::Unknown {
                session_id: session_id.to_string(),
            })?;
        if let Some(a) = agent_id {
            if s.agent_id != a {
                return Err(SessionError::AgentMismatch {
                    session_id: session_id.to_string(),
                    expected: s.agent_id.clone(),
                    got: a.to_string(),
                });
            }
        }
        g.sessions.remove(session_id);
        Ok(())
    }

    /// List sessions for an agent (optional context filter).
    pub fn list(&self, agent_id: &str, context_id: Option<&str>) -> Vec<TuneSessionSummary> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);
        g.sessions
            .values()
            .filter(|s| s.agent_id == agent_id)
            .filter(|s| context_id.is_none_or(|c| s.context_id == c))
            .map(|s| TuneSessionSummary {
                session_id: s.session_id.clone(),
                agent_id: s.agent_id.clone(),
                context_id: s.context_id.clone(),
                run_count: s.runs.len(),
                created_at_ms: s.created_at_ms,
            })
            .collect()
    }

    /// Append a run record to the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is missing or agent mismatch.
    pub fn append_run(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        run: TuneRunRecord,
    ) -> Result<(), String> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);
        let s = g
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("unknown session_id: {session_id}"))?;
        if let Some(a) = agent_id {
            if s.agent_id != a {
                return Err(format!(
                    "agent_id mismatch for session {session_id}: session is owned by {:?}, got {a:?}",
                    s.agent_id
                ));
            }
        }
        s.touch();
        if run.selected {
            for r in &mut s.runs {
                r.selected = false;
            }
        }
        s.runs.push(run);
        Ok(())
    }

    /// Mark a run as selected for export.
    ///
    /// # Errors
    ///
    /// Missing session/run or agent mismatch.
    pub fn select_run(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        run_id: &str,
    ) -> Result<(), String> {
        let mut g = self.inner.lock();
        Self::evict_expired(&mut g);
        let s = g
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("unknown session_id: {session_id}"))?;
        if let Some(a) = agent_id {
            if s.agent_id != a {
                return Err(format!(
                    "agent_id mismatch for session {session_id}: session is owned by {:?}, got {a:?}",
                    s.agent_id
                ));
            }
        }
        let mut found = false;
        for r in &mut s.runs {
            let sel = r.run_id == run_id;
            r.selected = sel;
            if sel {
                found = true;
            }
        }
        if !found {
            return Err(format!("unknown run_id: {run_id}"));
        }
        s.touch();
        Ok(())
    }

    /// Export winning params into a contexts.<id> artifact.
    ///
    /// # Errors
    ///
    /// Missing session or no runs to export.
    pub fn export(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
        context_id: Option<&str>,
    ) -> Result<TuneExportArtifact, String> {
        let sess = self
            .get(session_id, agent_id, context_id)
            .ok_or_else(|| "session not found".to_string())?;
        let run = sess
            .runs
            .iter()
            .rev()
            .find(|r| r.selected)
            .or_else(|| sess.runs.last())
            .ok_or_else(|| "no runs to export".to_string())?;

        let scope = run.params.get("scope").cloned().unwrap_or_else(|| {
            // Lift common scope fields from flat params
            let mut m = serde_json::Map::new();
            for key in [
                "weighted_phrases",
                "mode",
                "min_similarity",
                "scope_weight",
                "lexical_weight",
                "embed_band",
            ] {
                if let Some(v) = run.params.get(key) {
                    m.insert(key.to_string(), v.clone());
                }
            }
            serde_json::Value::Object(m)
        });

        let toml = render_context_scope_toml(&sess.context_id, &scope);
        let json = serde_json::json!({
            "session_id": sess.session_id,
            "agent_id": sess.agent_id,
            "context_id": sess.context_id,
            "scope": scope,
            "source_run_id": run.run_id,
        });

        Ok(TuneExportArtifact {
            session_id: sess.session_id,
            agent_id: sess.agent_id,
            context_id: sess.context_id,
            scope: scope.clone(),
            other: serde_json::json!({}),
            formats: TuneExportFormats { toml, json },
            source_run_id: Some(run.run_id.clone()),
        })
    }

    fn evict_expired(g: &mut Inner) {
        let ttl = g.ttl;
        g.sessions.retain(|_, s| s.last_touch.elapsed() < ttl);
    }
}

impl Default for TuneSessionStore {
    fn default() -> Self {
        Self::new(DEFAULT_SESSION_TTL_SECS)
    }
}

/// Render `[contexts.<id>.scope]` TOML fragment from a JSON scope object.
fn render_context_scope_toml(context_id: &str, scope: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&format!("[contexts.{context_id}.scope]\n"));

    if let Some(mode) = scope.get("mode").and_then(|v| v.as_str()) {
        out.push_str(&format!("mode = \"{mode}\"\n"));
    }
    if let Some(ms) = scope.get("min_similarity").and_then(|v| v.as_f64()) {
        out.push_str(&format!("min_similarity = {ms}\n"));
    }
    if let Some(sw) = scope.get("scope_weight").and_then(|v| v.as_f64()) {
        out.push_str(&format!("scope_weight = {sw}\n"));
    }
    if let Some(lw) = scope.get("lexical_weight").and_then(|v| v.as_f64()) {
        out.push_str(&format!("lexical_weight = {lw}\n"));
    }

    if let Some(arr) = scope.get("weighted_phrases").and_then(|v| v.as_array()) {
        for p in arr {
            out.push('\n');
            out.push_str(&format!(
                "[[contexts.{context_id}.scope.weighted_phrases]]\n"
            ));
            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                let escaped = t.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("text = \"{escaped}\"\n"));
            }
            if let Some(w) = p.get("weight").and_then(|v| v.as_f64()) {
                out.push_str(&format!("weight = {w}\n"));
            }
            if let Some(ms) = p.get("min_similarity").and_then(|v| v.as_f64()) {
                out.push_str(&format!("min_similarity = {ms}\n"));
            }
        }
    }

    out
}

#[cfg(test)]
mod unit_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn ttl_evicts_idle_sessions() {
        let store = TuneSessionStore::new(1);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        assert!(store.get(&s.session_id, Some("a"), Some("c")).is_none());
    }

    #[test]
    fn open_reuses_existing_session_id() {
        let store = TuneSessionStore::new(3600);
        let s1 = store.open("a".into(), "c".into(), None).unwrap();
        let s2 = store
            .open("a".into(), "c".into(), Some(s1.session_id.clone()))
            .unwrap();
        assert_eq!(s1.session_id, s2.session_id);
    }

    #[test]
    fn open_rejects_unknown_session_id() {
        let store = TuneSessionStore::new(3600);
        let err = store
            .open("a".into(), "c".into(), Some("nope".into()))
            .unwrap_err();
        assert!(err.contains("unknown session_id"));
    }

    #[test]
    fn open_rejects_wrong_agent_for_existing_session() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = store
            .open("b".into(), "c".into(), Some(s.session_id.clone()))
            .unwrap_err();
        assert!(
            err.contains("agent_id mismatch"),
            "expected 'agent_id mismatch' in {err}"
        );
    }

    #[test]
    fn open_rejects_wrong_context_for_existing_session() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = store
            .open("a".into(), "other".into(), Some(s.session_id.clone()))
            .unwrap_err();
        assert!(
            err.contains("context_id mismatch"),
            "expected 'context_id mismatch' in {err}"
        );
    }

    #[test]
    fn close_wrong_agent_returns_false() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        assert!(!store.close(&s.session_id, Some("b")));
    }

    #[test]
    fn close_nonexistent_returns_false() {
        let store = TuneSessionStore::new(3600);
        assert!(!store.close("nope", None));
    }

    #[test]
    fn close_no_agent_filter_removes() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        assert!(store.close(&s.session_id, None));
        assert!(store.get(&s.session_id, Some("a"), Some("c")).is_none());
    }

    #[test]
    fn list_filters_by_agent_and_context() {
        let store = TuneSessionStore::new(3600);
        let _s1 = store.open("a".into(), "c1".into(), None).unwrap();
        let _s2 = store.open("a".into(), "c2".into(), None).unwrap();
        let _s3 = store.open("b".into(), "c1".into(), None).unwrap();

        // agent a, no context filter → 2
        let list = store.list("a", None);
        assert_eq!(list.len(), 2);

        // agent a, context c1 → 1
        let list = store.list("a", Some("c1"));
        assert_eq!(list.len(), 1);

        // agent b, all → 1
        let list = store.list("b", None);
        assert_eq!(list.len(), 1);

        // agent z → 0
        let list = store.list("z", None);
        assert!(list.is_empty());
    }

    #[test]
    fn get_no_filters_touches_session() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        // None, None → accepts any
        assert!(store.get(&s.session_id, None, None).is_some());
    }

    #[test]
    fn append_run_agent_mismatch() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = store
            .append_run(
                &s.session_id,
                Some("b"),
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap_err();
        assert!(
            err.contains("agent_id mismatch"),
            "expected 'agent_id mismatch' in {err}"
        );
    }

    #[test]
    fn append_run_unknown_session() {
        let store = TuneSessionStore::new(3600);
        let err = store
            .append_run(
                "nope",
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap_err();
        assert!(err.contains("unknown session_id"));
    }

    #[test]
    fn append_run_selected_clears_prior_selections() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: true,
                },
            )
            .unwrap();
        // Second selected run clears first
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r2".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: true,
                },
            )
            .unwrap();
        let sess = store.get(&s.session_id, None, None).unwrap();
        assert!(!sess.runs[0].selected);
        assert!(sess.runs[1].selected);
    }

    #[test]
    fn select_run_happy_path() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r2".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        store.select_run(&s.session_id, None, "r1").unwrap();
        let sess = store.get(&s.session_id, None, None).unwrap();
        assert!(sess.runs[0].selected);
        assert!(!sess.runs[1].selected);
    }

    #[test]
    fn select_run_unknown_run_id() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = store.select_run(&s.session_id, None, "nope").unwrap_err();
        assert!(err.contains("unknown run_id"));
    }

    #[test]
    fn select_run_agent_mismatch() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        let err = store
            .select_run(&s.session_id, Some("b"), "r1")
            .unwrap_err();
        assert!(
            err.contains("agent_id mismatch"),
            "expected 'agent_id mismatch' in {err}"
        );
    }

    #[test]
    fn select_run_unknown_session() {
        let store = TuneSessionStore::new(3600);
        let err = store.select_run("nope", None, "r1").unwrap_err();
        assert!(err.contains("unknown session_id"));
    }

    #[test]
    fn export_no_runs_error() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        let err = store
            .export(&s.session_id, Some("a"), Some("c"))
            .unwrap_err();
        assert!(err.contains("no runs to export"));
    }

    #[test]
    fn export_session_not_found() {
        let store = TuneSessionStore::new(3600);
        let err = store.export("nope", None, None).unwrap_err();
        assert!(err.contains("session not found"));
    }

    #[test]
    fn export_prefers_selected_over_last() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({"mode": "filter", "min_similarity": 0.5}),
                    metrics: serde_json::json!({}),
                    selected: true,
                },
            )
            .unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r2".into(),
                    tool: "t".into(),
                    params: serde_json::json!({"mode": "boost", "min_similarity": 0.1}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        let art = store.export(&s.session_id, Some("a"), Some("c")).unwrap();
        assert_eq!(art.source_run_id.as_deref(), Some("r1"));
        assert!(art.formats.toml.contains("mode = \"filter\""));
    }

    #[test]
    fn export_falls_back_to_last_run() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({"mode": "rerank"}),
                    metrics: serde_json::json!({}),
                    selected: false,
                },
            )
            .unwrap();
        let art = store.export(&s.session_id, Some("a"), Some("c")).unwrap();
        assert_eq!(art.source_run_id.as_deref(), Some("r1"));
    }

    #[test]
    fn export_with_nested_scope_param() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("a".into(), "c".into(), None).unwrap();
        store
            .append_run(
                &s.session_id,
                None,
                TuneRunRecord {
                    run_id: "r1".into(),
                    tool: "t".into(),
                    params: serde_json::json!({
                        "scope": {"mode": "boost", "min_similarity": 0.3}
                    }),
                    metrics: serde_json::json!({}),
                    selected: true,
                },
            )
            .unwrap();
        let art = store.export(&s.session_id, Some("a"), Some("c")).unwrap();
        // Nested "scope" key should be used directly
        assert!(art.formats.toml.contains("mode = \"boost\""));
    }

    #[test]
    fn render_toml_escapes_quotes() {
        let scope = serde_json::json!({
            "mode": "filter",
            "min_similarity": 0.25,
            "weighted_phrases": [{"text": "say \"hi\"", "weight": 1.0}]
        });
        let t = render_context_scope_toml("docs", &scope);
        assert!(t.contains("contexts.docs.scope"));
        assert!(t.contains("say \\\"hi\\\""));
    }

    // -- SessionError / get_checked / close_with_reason ----------------

    #[test]
    fn get_checked_unknown_session() {
        let store = TuneSessionStore::new(3600);
        let err = store.get_checked("missing", None, None).unwrap_err();
        assert!(matches!(err, SessionError::Unknown { .. }));
    }

    #[test]
    fn get_checked_agent_mismatch_detail() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("alice".into(), "ctx".into(), None).unwrap();
        let err = store
            .get_checked(&s.session_id, Some("bob"), Some("ctx"))
            .unwrap_err();
        match err {
            SessionError::AgentMismatch { expected, got, .. } => {
                assert_eq!(expected, "alice");
                assert_eq!(got, "bob");
            }
            other => panic!("expected AgentMismatch, got {other:?}"),
        }
    }

    #[test]
    fn get_checked_context_mismatch_detail() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("alice".into(), "ctx".into(), None).unwrap();
        let err = store
            .get_checked(&s.session_id, Some("alice"), Some("other"))
            .unwrap_err();
        match err {
            SessionError::ContextMismatch { expected, got, .. } => {
                assert_eq!(expected, "ctx");
                assert_eq!(got, "other");
            }
            other => panic!("expected ContextMismatch, got {other:?}"),
        }
    }

    #[test]
    fn close_with_reason_distinguishes_unknown_from_mismatch() {
        let store = TuneSessionStore::new(3600);
        let s = store.open("alice".into(), "ctx".into(), None).unwrap();

        // Unknown id
        match store.close_with_reason("nope", None).unwrap_err() {
            SessionError::Unknown { session_id } => assert_eq!(session_id, "nope"),
            other => panic!("expected Unknown, got {other:?}"),
        }

        // Agent mismatch
        match store
            .close_with_reason(&s.session_id, Some("bob"))
            .unwrap_err()
        {
            SessionError::AgentMismatch { expected, got, .. } => {
                assert_eq!(expected, "alice");
                assert_eq!(got, "bob");
            }
            other => panic!("expected AgentMismatch, got {other:?}"),
        }

        // Match → ok
        assert!(store
            .close_with_reason(&s.session_id, Some("alice"))
            .is_ok());
    }

    #[test]
    fn session_error_display_includes_ids() {
        let e = SessionError::AgentMismatch {
            session_id: "s1".into(),
            expected: "alice".into(),
            got: "bob".into(),
        };
        let s = e.to_string();
        assert!(s.contains("s1"));
        assert!(s.contains("alice"));
        assert!(s.contains("bob"));
    }
}
