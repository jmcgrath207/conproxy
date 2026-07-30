//! corpus_gen — generate synthetic corpus JSONL files from Markdown templates.
//!
//! Reads templates from tests/corpus/templates/*.md (frontmatter + paragraphs
//! separated by `---`), fills `{product}` slots from a seeded word bank, and
//! writes JSONL to tests/corpus/data/.
//!
//! Deterministic (seeded RNG). Usage:
//!   cargo run --bin corpus_gen

use fake::rand::rngs::StdRng;
use fake::rand::SeedableRng;
use fake::Fake;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

// ---------------------------------------------------------------------------
// Invented vocabulary — not on the internet
// ---------------------------------------------------------------------------

const PRODUCTS: &[&str] = &[
    "Xyfalon Cache",
    "Qorbix Mesh",
    "Nexafold Index",
    "Vexalith Queue",
    "Plexaron Engine",
    "Aethor Search",
    "Pulvion Monitor",
    "Cryxel Cascade",
    "Drevon Sensor",
    "Fyron Router",
    "Helvion Store",
    "Ixion Bridge",
    "Junctar Proxy",
    "Krylon Sync",
    "Luxon View",
    "Morphix Store",
    "Nyxon Relay",
    "Orvion Cache",
    "Phyxon Query",
    "Rivox Index",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CorpusEntry {
    id: String,
    title: String,
    content: String,
    category: String,
    tags: Vec<String>,
    topic: String,
    overlap: bool,
}

#[derive(Serialize)]
struct QueryEntry {
    query: String,
    corpus: String,
    expected_topic: String,
    expected_min_results: usize,
}

#[derive(serde::Deserialize)]
struct Frontmatter {
    topic: String,
    query: String,
    title: String,
    #[serde(default)]
    keywords: Vec<String>,
    category: String,
}

#[derive(serde::Deserialize)]
struct TopicsToml {
    #[serde(default)]
    docs: Vec<TemplateRef>,
    #[serde(default)]
    tickets: Vec<TemplateRef>,
    #[serde(default)]
    code: Vec<TemplateRef>,
}

#[derive(serde::Deserialize)]
struct TemplateRef {
    template: String,
}

#[derive(Debug)]
struct LoadedTemplate {
    topic: String,
    query: String,
    title_template: String,
    keywords: Vec<String>,
    category: String,
    paragraphs: Vec<String>,
}

// ---------------------------------------------------------------------------
// Template loading
// ---------------------------------------------------------------------------

fn load_templates(
    templates_dir: &Path,
    toml_path: &Path,
) -> Result<HashMap<String, Vec<LoadedTemplate>>, String> {
    let toml_content =
        fs::read_to_string(toml_path).map_err(|e| format!("Failed to read topics.toml: {e}"))?;
    let topics: TopicsToml =
        toml::from_str(&toml_content).map_err(|e| format!("Failed to parse topics.toml: {e}"))?;

    let mut result: HashMap<String, Vec<LoadedTemplate>> = HashMap::new();
    result.insert("docs".to_string(), load_group(templates_dir, &topics.docs)?);
    result.insert(
        "tickets".to_string(),
        load_group(templates_dir, &topics.tickets)?,
    );
    result.insert("code".to_string(), load_group(templates_dir, &topics.code)?);
    Ok(result)
}

fn load_group(templates_dir: &Path, refs: &[TemplateRef]) -> Result<Vec<LoadedTemplate>, String> {
    let mut out = Vec::new();
    for ref_entry in refs {
        let path = templates_dir.join(&ref_entry.template);
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        // Split frontmatter from body
        let (frontmatter_str, body) = if content.starts_with("---\n") {
            let mut parts = content.splitn(3, "---\n");
            let _ = parts.next(); // first ---
            let fm = parts
                .next()
                .ok_or_else(|| format!("Missing frontmatter in {}", path.display()))?;
            let rest = parts
                .next()
                .ok_or_else(|| format!("Missing body after frontmatter in {}", path.display()))?;
            (fm.to_string(), rest.to_string())
        } else {
            let fm = format!(
                r#"---
topic: {}
query: ""
title: ""
keywords: []
category: unknown
"#,
                ref_entry.template.trim_end_matches(".md")
            );
            (fm, content.clone())
        };

        let fm: Frontmatter = serde_yaml::from_str(&frontmatter_str)
            .map_err(|e| format!("Failed to parse frontmatter in {}: {e}", path.display()))?;

        // Split body on --- into paragraphs
        let paragraphs: Vec<String> = body
            .split("---\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        if paragraphs.is_empty() {
            return Err(format!("No paragraphs found in {}", path.display()));
        }

        out.push(LoadedTemplate {
            topic: fm.topic,
            query: fm.query,
            title_template: fm.title,
            keywords: fm.keywords,
            category: fm.category,
            paragraphs,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Content assembly
// ---------------------------------------------------------------------------

fn assemble_docs(
    rng: &mut StdRng,
    template: &LoadedTemplate,
    corpus_name: &str,
    start_idx: usize,
) -> Vec<CorpusEntry> {
    let mut entries = Vec::new();
    let docs_per_topic = 5;
    for i in 0..docs_per_topic {
        let product = pick_product(rng);
        let title = template.title_template.replace("{product}", product);
        let tags: Vec<String> = template.keywords.iter().map(|s| s.to_string()).collect();
        let idx = start_idx + i;

        // Pick 4-5 paragraphs from pool, shuffle
        let count = (4.min(template.paragraphs.len()))..(template.paragraphs.len() + 1);
        let n: usize = count.fake_with_rng(rng);
        let n = n.min(template.paragraphs.len());
        let mut pool: Vec<usize> = (0..template.paragraphs.len()).collect();
        for j in (1..pool.len()).rev() {
            let k: usize = (0..=j).fake_with_rng(rng);
            pool.swap(j, k);
        }
        let chosen: Vec<&str> = pool[..n]
            .iter()
            .map(|&j| template.paragraphs[j].as_str())
            .collect();
        let content = chosen.join("\n\n").replace("{product}", product);

        entries.push(CorpusEntry {
            id: format!("{}-{:03}", corpus_name, idx),
            title,
            content,
            category: template.category.clone(),
            tags,
            topic: template.topic.clone(),
            overlap: idx < 10,
        });
    }
    entries
}

fn pick_product(rng: &mut StdRng) -> &'static str {
    let idx: usize = (0..PRODUCTS.len()).fake_with_rng(rng);
    PRODUCTS[idx]
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), String> {
    let templates_dir = Path::new("tests/corpus/templates");
    let toml_path = templates_dir.join("topics.toml");
    let out_dir = Path::new("tests/corpus/data");
    fs::create_dir_all(out_dir).map_err(|e| format!("create output dir: {e}"))?;

    let seed = [42u8; 32];
    let mut rng = StdRng::from_seed(seed);

    let all_templates = load_templates(templates_dir, &toml_path)?;
    let corpora_name = ["docs", "tickets", "code"];

    let mut all_queries = Vec::new();
    for &corpus_name in &corpora_name {
        let templates = match all_templates.get(corpus_name) {
            Some(t) => t,
            None => {
                eprintln!("No templates for {corpus_name}, skipping");
                continue;
            }
        };

        let mut all_entries = Vec::new();
        let mut idx = 0;

        for template in templates {
            let entries = assemble_docs(&mut rng, template, corpus_name, idx);
            idx = idx.saturating_add(entries.len());
            all_entries.extend(entries);

            all_queries.push(QueryEntry {
                query: template.query.clone(),
                corpus: corpus_name.to_string(),
                expected_topic: template.topic.clone(),
                expected_min_results: 3,
            });
        }

        // Write JSONL
        let filepath = out_dir.join(format!("{corpus_name}.jsonl"));
        let mut f = fs::File::create(&filepath).map_err(|e| format!("create {filepath:?}: {e}"))?;
        for entry in &all_entries {
            serde_json::to_writer(&mut f, entry).map_err(|e| format!("write entry: {e}"))?;
            f.write_all(b"\n")
                .map_err(|e| format!("write newline: {e}"))?;
        }
        println!(
            "Wrote {} {} to {}",
            all_entries.len(),
            corpus_name,
            filepath.display()
        );
    }

    // Write all queries
    let qpath = out_dir.join("queries.jsonl");
    let mut qf = fs::File::create(&qpath).map_err(|e| format!("create queries: {e}"))?;
    for q in &all_queries {
        serde_json::to_writer(&mut qf, q).map_err(|e| format!("write query: {e}"))?;
        qf.write_all(b"\n")
            .map_err(|e| format!("write q newline: {e}"))?;
    }
    println!("Wrote {} queries to {}", all_queries.len(), qpath.display());

    println!("Done.");
    Ok(())
}
