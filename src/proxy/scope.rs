//! Scope filtering for upstream results.
//!
//! Filters, boosts, or reranks upstream results based on weighted phrase
//! similarity so results stay relevant to the configured project scope.
//!
//! # Score C
//! - Per entry: `sim_e` must meet entry/context min to contribute.
//! - **filter** uses unweighted `best_sim = max(sim_e)` (weight ignored).
//! - **boost** / **rerank** may use phrase weights and `scope_weight`.

use super::types::SearchResult;
use crate::config::ProxyScopeConfig;

/// Filter mode for scope filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// Discard results below similarity threshold.
    #[default]
    Filter,
    /// Rerank results by blending score with scope similarity.
    Rerank,
    /// Multiply score for matching results.
    Boost,
}

impl From<&str> for FilterMode {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "filter" => FilterMode::Filter,
            "rerank" => FilterMode::Rerank,
            "boost" => FilterMode::Boost,
            _ => FilterMode::Filter,
        }
    }
}

/// Precomputed phrase entry for hot-path scoring.
#[derive(Debug, Clone)]
struct PhraseEntry {
    text_lower: String,
    word_set: std::collections::HashSet<String>,
    weight: f32,
    /// Entry floor; `None` → use filter `min_similarity`.
    min_similarity: Option<f32>,
}

/// Aggregate scope signals for one hit (Score C).
#[derive(Debug, Clone, Copy)]
struct ScopeSignals {
    /// Unweighted max sim over contributing entries.
    best_sim: f32,
    /// Max `sim * weight` over contributing entries.
    best_weighted: f32,
    /// Whether any entry contributed.
    any: bool,
}

impl ScopeSignals {
    const NONE: Self = Self {
        best_sim: 0.0,
        best_weighted: 0.0,
        any: false,
    };
}

/// Scope filter that processes search results based on phrase configuration.
#[derive(Debug, Clone)]
pub struct ScopeFilter {
    phrases: Vec<PhraseEntry>,
    /// Legacy bare texts (for `seeds()` / fingerprint callers).
    phrase_texts: Vec<String>,
    mode: FilterMode,
    /// Context default floor + filter threshold.
    min_similarity: f32,
    /// Blend / boost strength (`scope_weight` / `seed_weight`).
    scope_weight: f32,
    query_prefix: Option<String>,
    /// Lexical vs semantic blend weight when hybrid embed active.
    lexical_weight: f32,
    /// Lexical band `[lo, hi]` where semantic hybrid applies.
    embed_band: [f32; 2],
    /// Optional phrase embeddings (parallel to `phrases`) for hybrid scoring.
    phrase_embeddings: Option<Vec<Vec<f32>>>,
}

impl ScopeFilter {
    /// Create a new scope filter from configuration.
    pub fn from_config(config: &ProxyScopeConfig) -> Self {
        let mode = config
            .mode
            .as_ref()
            .map(|s| FilterMode::from(s.as_str()))
            .unwrap_or_default();

        let effective = config.effective_phrases();
        let phrase_texts: Vec<String> = effective.iter().map(|p| p.text.clone()).collect();
        let phrases = effective
            .into_iter()
            .map(|p| {
                let text_lower = p.text.to_lowercase();
                let word_set = text_lower.split_whitespace().map(str::to_string).collect();
                PhraseEntry {
                    text_lower,
                    word_set,
                    weight: if p.weight.is_finite() && p.weight >= 0.0 {
                        p.weight
                    } else {
                        1.0
                    },
                    min_similarity: p.min_similarity,
                }
            })
            .collect();

        Self {
            phrases,
            phrase_texts,
            mode,
            min_similarity: config.min_similarity(),
            scope_weight: config.scope_weight(),
            query_prefix: config.query_prefix.clone(),
            lexical_weight: config.lexical_weight(),
            embed_band: config.embed_band(),
            phrase_embeddings: None,
        }
    }

    /// Attach precomputed phrase embeddings (parallel to configured phrases).
    ///
    /// Length must match phrase count; mismatched length is ignored (lexical-only).
    #[must_use]
    pub fn with_phrase_embeddings(mut self, embeddings: Vec<Vec<f32>>) -> Self {
        if embeddings.len() == self.phrases.len() {
            self.phrase_embeddings = Some(embeddings);
        }
        self
    }

    /// Whether hybrid embed scoring is active (phrase vectors present).
    #[must_use]
    pub fn has_phrase_embeddings(&self) -> bool {
        self.phrase_embeddings.is_some()
    }

    /// Create an empty filter (no filtering).
    #[must_use]
    pub fn none() -> Self {
        Self {
            phrases: Vec::new(),
            phrase_texts: Vec::new(),
            mode: FilterMode::Filter,
            min_similarity: 0.0,
            scope_weight: 0.0,
            query_prefix: None,
            lexical_weight: 0.5,
            embed_band: [0.1, 0.55],
            phrase_embeddings: None,
        }
    }

    /// Check if filtering is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.phrases.is_empty()
    }

    /// Configured phrase texts (compat name: seeds).
    #[must_use]
    pub fn seeds(&self) -> &[String] {
        &self.phrase_texts
    }

    /// Phrase texts.
    #[must_use]
    pub fn phrases(&self) -> &[String] {
        &self.phrase_texts
    }

    /// Get the filter mode.
    #[must_use]
    pub fn mode(&self) -> FilterMode {
        self.mode
    }

    /// Get the query prefix if configured.
    #[must_use]
    pub fn query_prefix(&self) -> Option<&str> {
        self.query_prefix.as_deref()
    }

    /// Apply prefix to a query if configured.
    #[must_use]
    pub fn apply_prefix(&self, query: &str) -> String {
        match &self.query_prefix {
            Some(prefix) => format!("{prefix} {query}"),
            None => query.to_string(),
        }
    }

    /// Lexical similarity of one phrase text vs content (token recall + substring bonus).
    fn lexical_sim(
        phrase_lower: &str,
        phrase_words: &std::collections::HashSet<String>,
        content_lower: &str,
        content_words: &std::collections::HashSet<&str>,
    ) -> f32 {
        if phrase_words.is_empty() || content_words.is_empty() {
            return 0.0;
        }

        let phrase_refs: std::collections::HashSet<&str> =
            phrase_words.iter().map(String::as_str).collect();
        let overlap = phrase_refs.intersection(content_words).count();
        // Token recall: |overlap| / |phrase_tokens| — short phrases not crushed by long content.
        let recall = overlap as f32 / phrase_refs.len() as f32;

        let substring_bonus = if content_lower.contains(phrase_lower) {
            0.3
        } else {
            0.0
        };

        (recall + substring_bonus).clamp(0.0, 1.0)
    }

    /// Cosine similarity; 0.0 on dim mismatch or zero magnitude.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let mut dot = 0.0_f64;
        let mut mag_a = 0.0_f64;
        let mut mag_b = 0.0_f64;
        for (x, y) in a.iter().zip(b.iter()) {
            let x = f64::from(*x);
            let y = f64::from(*y);
            dot += x * y;
            mag_a += x * x;
            mag_b += y * y;
        }
        let denom = (mag_a * mag_b).sqrt();
        if denom == 0.0 {
            0.0
        } else {
            (dot / denom) as f32
        }
    }

    /// Hybrid sim: lexical always; blend with semantic when embeddings + band allow.
    fn hybrid_sim(&self, phrase_idx: usize, lexical: f32, content_emb: Option<&[f32]>) -> f32 {
        let Some(phrase_embs) = self.phrase_embeddings.as_ref() else {
            return lexical;
        };
        let Some(content_emb) = content_emb else {
            return lexical;
        };
        let Some(phrase_emb) = phrase_embs.get(phrase_idx) else {
            return lexical;
        };

        let [lo, hi] = self.embed_band;
        if lexical >= hi || lexical < lo {
            // Clear match or too weak: lexical-only (v1 skips embed outside band).
            return lexical;
        }

        let semantic = Self::cosine_similarity(content_emb, phrase_emb).clamp(0.0, 1.0);
        let lw = self.lexical_weight.clamp(0.0, 1.0);
        (lw * lexical + (1.0 - lw) * semantic).clamp(0.0, 1.0)
    }

    /// Score C signals for content against all phrases (lexical-only).
    fn signals_for(&self, content: &str) -> ScopeSignals {
        self.signals_for_emb(content, None)
    }

    /// Score C signals with optional content embedding for hybrid.
    fn signals_for_emb(&self, content: &str, content_emb: Option<&[f32]>) -> ScopeSignals {
        if self.phrases.is_empty() {
            return ScopeSignals {
                best_sim: 1.0,
                best_weighted: 1.0,
                any: true,
            };
        }

        let content_lower = content.to_lowercase();
        let content_words: std::collections::HashSet<&str> =
            content_lower.split_whitespace().collect();

        let mut best_sim = 0.0f32;
        let mut best_weighted = 0.0f32;
        let mut any = false;

        for (i, p) in self.phrases.iter().enumerate() {
            let lexical =
                Self::lexical_sim(&p.text_lower, &p.word_set, &content_lower, &content_words);
            let sim = self.hybrid_sim(i, lexical, content_emb);
            let floor = p.min_similarity.unwrap_or(self.min_similarity);
            if sim < floor {
                continue;
            }
            any = true;
            best_sim = best_sim.max(sim);
            best_weighted = best_weighted.max(sim * p.weight);
        }

        if !any {
            ScopeSignals::NONE
        } else {
            ScopeSignals {
                best_sim,
                best_weighted,
                any: true,
            }
        }
    }

    /// Unweighted max similarity (`best_sim`). Empty phrases → 1.0.
    ///
    /// Compat name for callers/tests; prefer thinking in Score C terms.
    #[must_use]
    pub fn max_seed_similarity(&self, content: &str) -> f32 {
        self.signals_for(content).best_sim
    }

    /// Score C unweighted best similarity (0 if no entry contributes).
    #[must_use]
    pub fn best_sim(&self, content: &str) -> f32 {
        let s = self.signals_for(content);
        if s.any {
            s.best_sim
        } else if self.phrases.is_empty() {
            1.0
        } else {
            0.0
        }
    }

    /// Hybrid best_sim when content embedding is known.
    #[must_use]
    pub fn best_sim_with_embedding(&self, content: &str, content_emb: Option<&[f32]>) -> f32 {
        let s = self.signals_for_emb(content, content_emb);
        if s.any {
            s.best_sim
        } else if self.phrases.is_empty() {
            1.0
        } else {
            0.0
        }
    }

    /// Filter results based on phrase similarity (Score C modes). Lexical-only.
    #[must_use]
    pub fn filter_results(&self, results: Vec<SearchResult>) -> Vec<SearchResult> {
        self.filter_results_hybrid(results, None)
    }

    /// Filter with optional per-result content embeddings (parallel slice).
    ///
    /// When `content_embeddings` is `Some`, index `i` is used for `results[i]`.
    /// Missing/short slices fall back to lexical-only for that hit.
    #[must_use]
    pub fn filter_results_hybrid(
        &self,
        results: Vec<SearchResult>,
        content_embeddings: Option<&[Vec<f32>]>,
    ) -> Vec<SearchResult> {
        if !self.is_enabled() {
            return results;
        }

        match self.mode {
            FilterMode::Filter => self.apply_filter(results, content_embeddings),
            FilterMode::Rerank => self.apply_rerank(results, content_embeddings),
            FilterMode::Boost => self.apply_boost(results, content_embeddings),
        }
    }

    fn emb_at(content_embeddings: Option<&[Vec<f32>]>, idx: usize) -> Option<&[f32]> {
        content_embeddings.and_then(|e| e.get(idx).map(Vec::as_slice))
    }

    /// Filter: drop if no contributor or `best_sim < min_similarity`. **Weight ignored.**
    fn apply_filter(
        &self,
        results: Vec<SearchResult>,
        content_embeddings: Option<&[Vec<f32>]>,
    ) -> Vec<SearchResult> {
        results
            .into_iter()
            .enumerate()
            .filter(|(i, r)| {
                let s = self.signals_for_emb(&r.content, Self::emb_at(content_embeddings, *i));
                s.any && s.best_sim >= self.min_similarity
            })
            .map(|(_, r)| r)
            .collect()
    }

    /// Rerank: blend upstream score with scope signal (uses weights via `best_weighted`).
    fn apply_rerank(
        &self,
        mut results: Vec<SearchResult>,
        content_embeddings: Option<&[Vec<f32>]>,
    ) -> Vec<SearchResult> {
        let w = self.scope_weight.clamp(0.0, 1.0);
        let max_weight = self
            .phrases
            .iter()
            .map(|p| p.weight)
            .fold(1.0f32, f32::max)
            .max(1.0);

        let mut scored: Vec<(SearchResult, f32)> = results
            .drain(..)
            .enumerate()
            .map(|(i, r)| {
                let s = self.signals_for_emb(&r.content, Self::emb_at(content_embeddings, i));
                let scope_signal = if s.any {
                    (s.best_weighted / max_weight).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let blended = (1.0 - w) * r.score + w * scope_signal;
                (r, blended)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .map(|(mut r, new_score)| {
                r.score = new_score;
                r
            })
            .collect()
    }

    /// Boost: if `best_sim >= min`, multiply score by `(1 + scope_weight * norm)`.
    fn apply_boost(
        &self,
        results: Vec<SearchResult>,
        content_embeddings: Option<&[Vec<f32>]>,
    ) -> Vec<SearchResult> {
        let w = self.scope_weight.clamp(0.0, 1.0);
        let max_weight = self
            .phrases
            .iter()
            .map(|p| p.weight)
            .fold(1.0f32, f32::max)
            .max(1.0);

        results
            .into_iter()
            .enumerate()
            .map(|(i, mut r)| {
                let s = self.signals_for_emb(&r.content, Self::emb_at(content_embeddings, i));
                if s.any && s.best_sim >= self.min_similarity {
                    let norm = (s.best_weighted / max_weight).clamp(0.0, 1.0);
                    r.score *= 1.0 + w * norm;
                }
                r
            })
            .collect()
    }
}

/// Statistics from filtering operation.
#[derive(Debug, Clone, Default)]
pub struct FilterStats {
    /// Number of results before filtering.
    pub input_count: usize,
    /// Number of results after filtering.
    pub output_count: usize,
    /// Number of results filtered out.
    pub filtered_count: usize,
    /// Average seed similarity of kept results.
    pub avg_similarity: f32,
}

impl ScopeFilter {
    /// Filter results and return statistics.
    pub fn filter_with_stats(
        &self,
        results: Vec<SearchResult>,
    ) -> (Vec<SearchResult>, FilterStats) {
        let input_count = results.len();

        if !self.is_enabled() {
            let stats = FilterStats {
                input_count,
                output_count: input_count,
                filtered_count: 0,
                avg_similarity: 1.0,
            };
            return (results, stats);
        }

        let filtered = self.filter_results(results);
        let output_count = filtered.len();

        let avg_similarity = if filtered.is_empty() {
            0.0
        } else {
            let sum: f32 = filtered
                .iter()
                .map(|r| self.max_seed_similarity(&r.content))
                .sum();
            sum / filtered.len() as f32
        };

        let stats = FilterStats {
            input_count,
            output_count,
            filtered_count: input_count.saturating_sub(output_count),
            avg_similarity,
        };

        (filtered, stats)
    }

    /// Filter results and return both kept and discarded results.
    ///
    /// Useful for debug logging to see which results were filtered out.
    /// Returns (kept_results, discarded_results).
    pub fn filter_with_discarded(
        &self,
        results: Vec<SearchResult>,
    ) -> (Vec<SearchResult>, Vec<DiscardedResult>) {
        if !self.is_enabled() {
            return (results, Vec::new());
        }

        let mut kept = Vec::new();
        let mut discarded = Vec::new();

        for result in results {
            let s = self.signals_for(&result.content);
            if s.any && s.best_sim >= self.min_similarity {
                kept.push(result);
            } else {
                discarded.push(DiscardedResult {
                    id: result.id,
                    similarity: s.best_sim,
                    reason: DiscardReason::BelowThreshold {
                        actual: s.best_sim,
                        threshold: self.min_similarity,
                    },
                });
            }
        }

        (kept, discarded)
    }
}

/// A result that was filtered out with reason.
#[derive(Debug, Clone)]
pub struct DiscardedResult {
    /// The ID of the discarded result.
    pub id: String,
    /// The similarity score that was calculated.
    pub similarity: f32,
    /// Why the result was discarded.
    pub reason: DiscardReason,
}

/// Reason why a result was discarded.
#[derive(Debug, Clone)]
pub enum DiscardReason {
    /// Similarity score was below the configured threshold.
    BelowThreshold {
        /// The actual similarity score.
        actual: f32,
        /// The required threshold.
        threshold: f32,
    },
}

#[cfg(test)]
#[path = "tests/scope_tests.rs"]
mod tests;
