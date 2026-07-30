use super::data::{DocIndex, EvalQuery};

/// Score for a single expected document in a response.
pub struct DocMatchScore {
    pub doc_id: String,
    pub matched: bool,
    pub confidence: f64,
    pub matched_terms: Vec<String>,
}

/// Recall score for a single query.
#[allow(dead_code)]
pub struct QueryRecallScore {
    pub query_id: String,
    pub expected_doc_count: usize,
    pub matched_doc_count: usize,
    pub recall: f64,
    pub doc_scores: Vec<DocMatchScore>,
}

/// Score a Claude response against expected documents.
///
/// Uses a 4-layer weighted content matching algorithm:
/// 1. Title match (weight 3.0)
/// 2. Key phrases / distinctive bigrams (weight 2.0 each, max 5)
/// 3. Tag terms (weight 1.0 each)
/// 4. Content terms (weight 0.5 each)
///
/// A document is considered "matched" when confidence >= 0.3.
pub fn score_response(response: &str, query: &EvalQuery, doc_index: &DocIndex) -> QueryRecallScore {
    let response_lower = response.to_lowercase();

    let doc_scores: Vec<DocMatchScore> = query
        .expected_doc_ids
        .iter()
        .map(|doc_id| score_document(doc_id, &response_lower, doc_index))
        .collect();

    let matched_count = doc_scores.iter().filter(|s| s.matched).count();
    let expected_count = query.expected_doc_ids.len();
    let recall = if expected_count > 0 {
        matched_count as f64 / expected_count as f64
    } else {
        1.0
    };

    QueryRecallScore {
        query_id: query.id.clone(),
        expected_doc_count: expected_count,
        matched_doc_count: matched_count,
        recall,
        doc_scores,
    }
}

fn score_document(doc_id: &str, response_lower: &str, doc_index: &DocIndex) -> DocMatchScore {
    let doc = match doc_index.docs.get(doc_id) {
        Some(d) => d,
        None => {
            return DocMatchScore {
                doc_id: doc_id.to_string(),
                matched: false,
                confidence: 0.0,
                matched_terms: vec![],
            };
        }
    };

    let mut total_weight = 0.0_f64;
    let mut matched_weight = 0.0_f64;
    let mut matched_terms = Vec::new();

    // Layer 1: Title match (weight 3.0)
    let title_weight = 3.0;
    total_weight += title_weight;

    let title_lower = doc.title.to_lowercase();
    if response_lower.contains(&title_lower) {
        // Full title match
        matched_weight += title_weight;
        matched_terms.push(format!("title:{}", doc.title));
    } else {
        // Partial title match: 60%+ of significant words
        let significant_words: Vec<&str> = title_lower
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .collect();
        if !significant_words.is_empty() {
            let matches = significant_words
                .iter()
                .filter(|w| response_lower.contains(*w))
                .count();
            let ratio = matches as f64 / significant_words.len() as f64;
            if ratio >= 0.6 {
                matched_weight += title_weight * 0.67; // partial credit
                matched_terms.push(format!(
                    "title-partial:{}/{}",
                    matches,
                    significant_words.len()
                ));
            }
        }
    }

    // Layer 2: Key phrases / distinctive bigrams (weight 2.0 each, max 5)
    let phrases = extract_key_phrases(&doc.content);
    let phrase_max = 5.min(phrases.len());
    for phrase in phrases.iter().take(phrase_max) {
        let phrase_weight = 2.0;
        total_weight += phrase_weight;
        let phrase_lower = phrase.to_lowercase();
        if response_lower.contains(&phrase_lower) {
            matched_weight += phrase_weight;
            matched_terms.push(format!("phrase:{phrase}"));
        }
    }

    // Layer 3: Tag terms (weight 1.0 each)
    for tag in &doc.tags {
        let tag_weight = 1.0;
        total_weight += tag_weight;
        let tag_lower = tag.to_lowercase();
        // Match tag directly or with spaces instead of hyphens
        let tag_spaced = tag_lower.replace('-', " ");
        if response_lower.contains(&tag_lower) || response_lower.contains(&tag_spaced) {
            matched_weight += tag_weight;
            matched_terms.push(format!("tag:{tag}"));
        }
    }

    // Layer 4: Content terms (weight 0.5 each)
    if let Some(terms) = doc_index.key_terms.get(doc_id) {
        for term in terms.iter().take(10) {
            let term_weight = 0.5;
            total_weight += term_weight;
            if response_lower.contains(&term.to_lowercase()) {
                matched_weight += term_weight;
                matched_terms.push(format!("term:{term}"));
            }
        }
    }

    let confidence = if total_weight > 0.0 {
        matched_weight / total_weight
    } else {
        0.0
    };

    DocMatchScore {
        doc_id: doc_id.to_string(),
        matched: confidence >= 0.3,
        confidence,
        matched_terms,
    }
}

/// Extract distinctive bigrams from document content.
///
/// Returns multi-word phrases that are likely to be unique identifiers
/// (proper nouns, technical terms, compound names).
fn extract_key_phrases(content: &str) -> Vec<String> {
    let words: Vec<&str> = content.split_whitespace().collect();
    let mut phrases = Vec::new();

    for window in words.windows(2) {
        let w1 = window[0];
        let w2 = window[1];

        // Skip if either word is a common stop word or too short
        if w1.len() <= 3 || w2.len() <= 3 {
            continue;
        }

        // Keep phrases where at least one word starts with uppercase
        // (likely a proper noun or technical term) or contains special chars
        let is_distinctive = w1.starts_with(|c: char| c.is_uppercase())
            || w2.starts_with(|c: char| c.is_uppercase())
            || w1.contains('-')
            || w2.contains('-')
            || w1.contains('_')
            || w2.contains('_');

        if is_distinctive {
            // Clean punctuation from edges
            let clean1 = w1.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            let clean2 = w2.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if !clean1.is_empty() && !clean2.is_empty() {
                phrases.push(format!("{clean1} {clean2}"));
            }
        }
    }

    // Deduplicate and take most distinctive
    phrases.sort();
    phrases.dedup();
    phrases
}
