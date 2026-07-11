use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::code_tokenizer::{CodeTokenizer, TokenContext};
use crate::min_heap::MinHeap;

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

pub type SnapshotPostingEntry = (String, Vec<usize>, usize);
pub type SnapshotPostingList = (String, Vec<SnapshotPostingEntry>);

#[derive(Clone, Debug)]
struct PostingEntry {
    lines: Vec<usize>,
    term_freq: usize,
    best_context: TokenContext,
}

#[derive(Clone, Debug)]
struct DocumentMeta {
    total_tokens: usize,
    last_modified: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct IndexSearchHit {
    pub file_path: String,
    pub score: f64,
    pub hit_lines: Vec<usize>,
    pub matched_tokens: Vec<String>,
    pub best_context: TokenContext,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvertedIndexSnapshot {
    pub postings: Vec<SnapshotPostingList>,
    pub doc_meta: Vec<(String, usize, Option<u64>)>,
    pub total_docs: usize,
    pub avg_doc_len: f64,
}

pub struct InvertedIndex {
    postings: HashMap<String, HashMap<String, PostingEntry>>,
    doc_meta: HashMap<String, DocumentMeta>,
    file_tokens: HashMap<String, Vec<String>>,
    total_docs: usize,
    avg_doc_len: f64,
    tokenizer: CodeTokenizer,
    ready: bool,
}

impl Default for InvertedIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl InvertedIndex {
    pub fn new() -> Self {
        Self {
            postings: HashMap::new(),
            doc_meta: HashMap::new(),
            file_tokens: HashMap::new(),
            total_docs: 0,
            avg_doc_len: 0.0,
            tokenizer: CodeTokenizer::new(),
            ready: false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn build_from_files(&mut self, project_root: &str, files: &[(String, String)]) {
        self.postings.clear();
        self.doc_meta.clear();
        self.file_tokens.clear();
        self.total_docs = 0;
        self.avg_doc_len = 0.0;

        for (path, file_type) in files {
            let full_path = format!("{}/{}", project_root, path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                self.add_document(path, &content, file_type);
            }
        }

        self.recalculate_stats();
        self.ready = true;
    }

    pub fn add_document(&mut self, file_path: &str, content: &str, _file_type: &str) {
        let result = self.tokenizer.tokenize_file(file_path, content);

        let last_modified = std::fs::metadata(file_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);

        self.doc_meta.insert(
            file_path.to_string(),
            DocumentMeta {
                total_tokens: result.total_tokens,
                last_modified,
            },
        );

        let mut file_term_set = Vec::new();
        let mut term_data: HashMap<String, (Vec<usize>, usize, TokenContext)> = HashMap::new();

        for tok in &result.tokens {
            let entry = term_data
                .entry(tok.token.clone())
                .or_insert_with(|| (Vec::new(), 0, tok.context));
            entry.0.push(tok.line);
            entry.1 += 1;
            if context_priority(tok.context) > context_priority(entry.2) {
                entry.2 = tok.context;
            }
        }

        for (term, (lines, freq, best_ctx)) in term_data {
            file_term_set.push(term.clone());
            let posting_map = self.postings.entry(term).or_default();
            posting_map.insert(
                file_path.to_string(),
                PostingEntry {
                    lines,
                    term_freq: freq,
                    best_context: best_ctx,
                },
            );
        }

        self.file_tokens
            .insert(file_path.to_string(), file_term_set);
        self.total_docs += 1;
    }

    pub fn remove_file(&mut self, file_path: &str) {
        if let Some(terms) = self.file_tokens.remove(file_path) {
            for term in terms {
                if let Some(posting_map) = self.postings.get_mut(&term) {
                    posting_map.remove(file_path);
                    if posting_map.is_empty() {
                        self.postings.remove(&term);
                    }
                }
            }
        }
        self.doc_meta.remove(file_path);
        if self.total_docs > 0 {
            self.total_docs -= 1;
        }
        self.recalculate_stats();
    }

    pub fn update_file(&mut self, project_root: &str, file_path: &str, file_type: &str) {
        self.remove_file(file_path);
        let full_path = format!("{}/{}", project_root, file_path);
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            self.add_document(file_path, &content, file_type);
            self.recalculate_stats();
        }
    }

    pub fn search(&self, query_tokens: &[String], max_results: usize) -> Vec<IndexSearchHit> {
        if query_tokens.is_empty() || !self.ready {
            return Vec::new();
        }

        let mut file_scores: HashMap<String, (f64, Vec<usize>, Vec<String>, TokenContext)> =
            HashMap::new();

        for token in query_tokens {
            if let Some(posting_map) = self.postings.get(token) {
                let df = posting_map.len();
                let idf =
                    ((self.total_docs as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();

                for (file_path, entry) in posting_map {
                    let doc_len = self
                        .doc_meta
                        .get(file_path)
                        .map(|m| m.total_tokens)
                        .unwrap_or(1) as f64;

                    let tf = entry.term_freq as f64;
                    let tf_norm = (tf * (BM25_K1 + 1.0))
                        / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len / self.avg_doc_len));

                    let bm25_score = idf * tf_norm;

                    let e = file_scores
                        .entry(file_path.clone())
                        .or_insert_with(|| (0.0, Vec::new(), Vec::new(), TokenContext::Usage));

                    e.0 += bm25_score;
                    e.1.extend_from_slice(&entry.lines);
                    e.2.push(token.clone());

                    if context_priority(entry.best_context) > context_priority(e.3) {
                        e.3 = entry.best_context;
                    }
                }
            }
        }

        // proximity boost
        if query_tokens.len() >= 2 {
            for (_file_path, (score, lines, matched, _)) in file_scores.iter_mut() {
                if matched.len() >= 2 {
                    let mut sorted_lines = lines.clone();
                    sorted_lines.sort();
                    sorted_lines.dedup();

                    let mut proximity_bonus = 0.0;
                    for i in 1..sorted_lines.len() {
                        let gap = sorted_lines[i] - sorted_lines[i - 1];
                        if gap <= 5 {
                            proximity_bonus += 0.1 * (5.0 - gap as f64) / 5.0;
                        }
                    }

                    *score *= 1.0 + proximity_bonus.min(0.5);
                }

                // coverage bonus
                let coverage = matched.len() as f64 / query_tokens.len() as f64;
                if coverage > 0.5 {
                    *score *= 1.0 + (coverage - 0.5) * 0.4;
                }
            }
        }

        let cmp = |a: &IndexSearchHit, b: &IndexSearchHit| -> std::cmp::Ordering {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        };
        let mut heap = MinHeap::new(max_results, cmp);

        for (file_path, (score, mut lines, matched, best_ctx)) in file_scores {
            lines.sort();
            lines.dedup();
            heap.push(IndexSearchHit {
                file_path,
                score,
                hit_lines: lines,
                matched_tokens: matched,
                best_context: best_ctx,
            });
        }

        heap.into_sorted_desc()
    }

    pub fn get_document_meta(&self, file_path: &str) -> Option<u64> {
        self.doc_meta.get(file_path).and_then(|m| m.last_modified)
    }

    pub fn get_stats(&self) -> InvertedIndexStats {
        InvertedIndexStats {
            total_documents: self.total_docs,
            unique_tokens: self.postings.len(),
            avg_doc_len: self.avg_doc_len,
        }
    }

    pub fn to_snapshot(&self) -> InvertedIndexSnapshot {
        let postings = self
            .postings
            .iter()
            .map(|(term, docs)| {
                let entries: Vec<(String, Vec<usize>, usize)> = docs
                    .iter()
                    .map(|(path, entry)| (path.clone(), entry.lines.clone(), entry.term_freq))
                    .collect();
                (term.clone(), entries)
            })
            .collect();

        let doc_meta = self
            .doc_meta
            .iter()
            .map(|(path, meta)| (path.clone(), meta.total_tokens, meta.last_modified))
            .collect();

        InvertedIndexSnapshot {
            postings,
            doc_meta,
            total_docs: self.total_docs,
            avg_doc_len: self.avg_doc_len,
        }
    }

    pub fn from_snapshot(&mut self, snapshot: InvertedIndexSnapshot) {
        self.postings.clear();
        self.doc_meta.clear();
        self.file_tokens.clear();

        for (term, entries) in snapshot.postings {
            let mut doc_map = HashMap::new();
            for (path, lines, freq) in entries {
                self.file_tokens
                    .entry(path.clone())
                    .or_default()
                    .push(term.clone());
                doc_map.insert(
                    path,
                    PostingEntry {
                        lines,
                        term_freq: freq,
                        best_context: TokenContext::Usage,
                    },
                );
            }
            self.postings.insert(term, doc_map);
        }

        for (path, total_tokens, last_modified) in snapshot.doc_meta {
            self.doc_meta.insert(
                path,
                DocumentMeta {
                    total_tokens,
                    last_modified,
                },
            );
        }

        self.total_docs = snapshot.total_docs;
        self.avg_doc_len = snapshot.avg_doc_len;
        self.ready = true;
    }

    fn recalculate_stats(&mut self) {
        self.total_docs = self.doc_meta.len();
        if self.total_docs > 0 {
            let total: usize = self.doc_meta.values().map(|m| m.total_tokens).sum();
            self.avg_doc_len = total as f64 / self.total_docs as f64;
        } else {
            self.avg_doc_len = 0.0;
        }
    }
}

#[derive(Clone, Debug)]
pub struct InvertedIndexStats {
    pub total_documents: usize,
    pub unique_tokens: usize,
    pub avg_doc_len: f64,
}

fn context_priority(ctx: TokenContext) -> u8 {
    match ctx {
        TokenContext::Definition => 4,
        TokenContext::Import => 3,
        TokenContext::Usage => 2,
        TokenContext::String => 1,
        TokenContext::Comment => 0,
    }
}
