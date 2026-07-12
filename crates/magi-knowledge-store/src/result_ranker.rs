use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::code_tokenizer::TokenContext;
use crate::inverted_index::IndexSearchHit;
use crate::min_heap::MinHeap;
use crate::symbol_index::SymbolSearchHit;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankWeights {
    pub tfidf: f64,
    pub symbol_match: f64,
    pub position_weight: f64,
    pub centrality: f64,
    pub recency: f64,
    pub type_weight: f64,
}

impl Default for RankWeights {
    fn default() -> Self {
        Self {
            tfidf: 0.30,
            symbol_match: 0.28,
            position_weight: 0.15,
            centrality: 0.10,
            recency: 0.07,
            type_weight: 0.10,
        }
    }
}

impl RankWeights {
    fn normalized(&self) -> Self {
        let total = self.tfidf
            + self.symbol_match
            + self.position_weight
            + self.centrality
            + self.recency
            + self.type_weight;
        if total <= 0.0 {
            return self.clone();
        }
        Self {
            tfidf: self.tfidf / total,
            symbol_match: self.symbol_match / total,
            position_weight: self.position_weight / total,
            centrality: self.centrality / total,
            recency: self.recency / total,
            type_weight: self.type_weight / total,
        }
    }

    fn merge_overrides(&self, overrides: &RankWeights) -> Self {
        Self {
            tfidf: if overrides.tfidf > 0.0 {
                overrides.tfidf
            } else {
                self.tfidf
            },
            symbol_match: if overrides.symbol_match > 0.0 {
                overrides.symbol_match
            } else {
                self.symbol_match
            },
            position_weight: if overrides.position_weight > 0.0 {
                overrides.position_weight
            } else {
                self.position_weight
            },
            centrality: if overrides.centrality > 0.0 {
                overrides.centrality
            } else {
                self.centrality
            },
            recency: if overrides.recency > 0.0 {
                overrides.recency
            } else {
                self.recency
            },
            type_weight: if overrides.type_weight > 0.0 {
                overrides.type_weight
            } else {
                self.type_weight
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScoreDimensions {
    pub tfidf: f64,
    pub symbol_match: f64,
    pub position_weight: f64,
    pub centrality: f64,
    pub recency: f64,
    pub type_weight: f64,
    pub recent_edit_boost: f64,
    pub scope_boost: f64,
}

#[derive(Clone, Debug)]
pub struct RankedResult {
    pub file_path: String,
    pub final_score: f64,
    pub breakdown: ScoreDimensions,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RankBoostSignals {
    pub preferred_scopes: Vec<String>,
    pub recent_edited_files: HashSet<String>,
}

struct FileScoreEntry {
    sources: HashSet<String>,
    tfidf: f64,
    symbol_match: f64,
    position_weight: f64,
    centrality: f64,
    recency: f64,
    type_weight: f64,
    recent_edit_boost: f64,
    scope_boost: f64,
}

impl Default for FileScoreEntry {
    fn default() -> Self {
        Self {
            sources: HashSet::new(),
            tfidf: 0.0,
            symbol_match: 0.0,
            position_weight: 0.0,
            centrality: 0.0,
            recency: 0.0,
            type_weight: 0.0,
            recent_edit_boost: 0.0,
            scope_boost: 0.0,
        }
    }
}

pub struct ResultRanker {
    weights: RankWeights,
}

pub struct RankInput<'a> {
    pub index_hits: &'a [IndexSearchHit],
    pub symbol_hits: &'a [SymbolSearchHit],
    pub centrality_map: &'a HashMap<String, f64>,
    pub max_results: usize,
    pub file_timestamps: &'a HashMap<String, u64>,
    pub weight_overrides: Option<&'a RankWeights>,
    pub boost_signals: Option<&'a RankBoostSignals>,
}

impl Default for ResultRanker {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ResultRanker {
    pub fn new(weights: Option<RankWeights>) -> Self {
        let w = weights.unwrap_or_default().normalized();
        Self { weights: w }
    }

    pub fn rank(&self, input: RankInput<'_>) -> Vec<RankedResult> {
        let effective_weights = match input.weight_overrides {
            Some(overrides) => self.weights.merge_overrides(overrides).normalized(),
            None => self.weights.clone(),
        };

        let mut file_map: HashMap<String, FileScoreEntry> = HashMap::new();

        for hit in input.index_hits {
            let entry = file_map.entry(hit.file_path.clone()).or_default();
            entry.sources.insert("index".to_string());
            entry.tfidf = entry.tfidf.max(hit.score);
            entry.position_weight = entry
                .position_weight
                .max(context_to_weight(hit.best_context));
        }

        for hit in input.symbol_hits {
            let entry = file_map.entry(hit.symbol.file_path.clone()).or_default();
            entry.sources.insert("symbol".to_string());
            entry.symbol_match = entry.symbol_match.max(hit.score);
            if hit.symbol.is_exported {
                entry.type_weight = entry.type_weight.max(0.3);
            }
        }

        for (file_path, entry) in file_map.iter_mut() {
            if let Some(&c) = input.centrality_map.get(file_path) {
                entry.centrality = c;
            }
        }

        let now = now_millis();
        for (file_path, entry) in file_map.iter_mut() {
            if let Some(&mtime) = input.file_timestamps.get(file_path) {
                entry.recency = calculate_recency(mtime, now);
            }
        }

        if let Some(signals) = input.boost_signals {
            if !signals.recent_edited_files.is_empty() {
                for (file_path, entry) in file_map.iter_mut() {
                    if signals.recent_edited_files.contains(file_path) {
                        entry.recent_edit_boost = 1.0;
                    }
                }
            }
            if !signals.preferred_scopes.is_empty() {
                for (file_path, entry) in file_map.iter_mut() {
                    entry.scope_boost = calculate_scope_boost(file_path, &signals.preferred_scopes);
                }
            }
        }

        let cmp = |a: &RankedResult, b: &RankedResult| -> std::cmp::Ordering {
            a.final_score
                .partial_cmp(&b.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.file_path.cmp(&a.file_path))
        };
        let mut heap = MinHeap::new(input.max_results, cmp);

        for (file_path, entry) in file_map {
            let breakdown = ScoreDimensions {
                tfidf: entry.tfidf,
                symbol_match: entry.symbol_match,
                position_weight: entry.position_weight,
                centrality: entry.centrality,
                recency: entry.recency,
                type_weight: entry.type_weight,
                recent_edit_boost: entry.recent_edit_boost,
                scope_boost: entry.scope_boost,
            };

            let mut final_score = breakdown.tfidf * effective_weights.tfidf
                + breakdown.symbol_match * effective_weights.symbol_match
                + breakdown.position_weight * effective_weights.position_weight
                + breakdown.centrality * effective_weights.centrality
                + breakdown.recency * effective_weights.recency
                + breakdown.type_weight * effective_weights.type_weight;

            let scope_bonus = breakdown.scope_boost * 0.15;
            let recent_edit_bonus = breakdown.recent_edit_boost * 0.10;
            final_score *= 1.0 + scope_bonus + recent_edit_bonus;

            if entry.sources.len() >= 3 {
                final_score *= 1.20;
            } else if entry.sources.len() >= 2 {
                final_score *= 1.10;
            }

            heap.push(RankedResult {
                file_path,
                final_score,
                breakdown,
                sources: entry.sources.into_iter().collect(),
            });
        }

        heap.into_sorted_desc()
    }
}

fn context_to_weight(ctx: TokenContext) -> f64 {
    match ctx {
        TokenContext::Definition => 1.0,
        TokenContext::Import => 0.6,
        TokenContext::Usage => 0.4,
        TokenContext::String => 0.2,
        TokenContext::Comment => 0.2,
    }
}

fn calculate_recency(last_modified: u64, now: u64) -> f64 {
    let age_hours = (now.saturating_sub(last_modified)) as f64 / (1000.0 * 60.0 * 60.0);
    let half_life = 72.0;
    0.5_f64.powf(age_hours / half_life)
}

fn calculate_scope_boost(file_path: &str, preferred_scopes: &[String]) -> f64 {
    let norm_fp = file_path.replace('\\', "/").to_lowercase();
    let mut best = 0.0_f64;

    for scope in preferred_scopes {
        let norm_scope = scope
            .replace('\\', "/")
            .trim_start_matches("./")
            .to_lowercase();
        if norm_scope.is_empty() {
            continue;
        }

        if norm_fp == norm_scope {
            best = best.max(1.0);
        } else if norm_fp.starts_with(&format!("{}/", norm_scope)) {
            best = best.max(0.8);
        } else if norm_fp.contains(&format!("/{}/", norm_scope))
            || norm_fp.ends_with(&format!("/{}", norm_scope))
        {
            best = best.max(0.4);
        }
    }

    best
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_index::{MatchType, SymbolEntry, SymbolKind};

    #[test]
    fn test_weight_normalization() {
        let w = RankWeights::default().normalized();
        let total =
            w.tfidf + w.symbol_match + w.position_weight + w.centrality + w.recency + w.type_weight;
        assert!((total - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_rank_basic() {
        let ranker = ResultRanker::new(None);
        let index_hits = vec![IndexSearchHit {
            file_path: "src/auth.rs".to_string(),
            score: 5.0,
            hit_lines: vec![10, 20],
            matched_tokens: vec!["auth".to_string()],
            best_context: TokenContext::Definition,
        }];
        let symbol_hits = vec![SymbolSearchHit {
            symbol: SymbolEntry {
                name: "authenticate".to_string(),
                kind: SymbolKind::Function,
                file_path: "src/auth.rs".to_string(),
                line: 10,
                end_line: Some(30),
                is_exported: true,
                container: None,
                signature: None,
            },
            score: 80.0,
            match_type: MatchType::Exact,
        }];
        let results = ranker.rank(RankInput {
            index_hits: &index_hits,
            symbol_hits: &symbol_hits,
            centrality_map: &HashMap::new(),
            max_results: 10,
            file_timestamps: &HashMap::new(),
            weight_overrides: None,
            boost_signals: None,
        });
        assert_eq!(results.len(), 1);
        assert!(results[0].final_score > 0.0);
        assert!(results[0].sources.len() >= 2);
    }

    #[test]
    fn test_cross_source_bonus() {
        let ranker = ResultRanker::new(None);
        let index_hits = vec![IndexSearchHit {
            file_path: "a.rs".to_string(),
            score: 1.0,
            hit_lines: vec![1],
            matched_tokens: vec!["x".to_string()],
            best_context: TokenContext::Usage,
        }];
        let symbol_hits = vec![SymbolSearchHit {
            symbol: SymbolEntry {
                name: "x".to_string(),
                kind: SymbolKind::Function,
                file_path: "a.rs".to_string(),
                line: 1,
                end_line: None,
                is_exported: false,
                container: None,
                signature: None,
            },
            score: 1.0,
            match_type: MatchType::Exact,
        }];
        let mut centrality = HashMap::new();
        centrality.insert("a.rs".to_string(), 0.5);
        let r2 = ranker.rank(RankInput {
            index_hits: &index_hits,
            symbol_hits: &symbol_hits,
            centrality_map: &centrality,
            max_results: 10,
            file_timestamps: &HashMap::new(),
            weight_overrides: None,
            boost_signals: None,
        });
        assert!(r2[0].sources.len() >= 2);
    }

    #[test]
    fn test_recency() {
        let now = now_millis();
        let recent = calculate_recency(now - 1000, now);
        let old = calculate_recency(now - 72 * 3600 * 1000, now);
        assert!(recent > old);
        assert!((old - 0.5).abs() < 0.05);
    }

    #[test]
    fn test_scope_boost() {
        assert_eq!(
            calculate_scope_boost("src/auth.rs", &["src/auth.rs".into()]),
            1.0
        );
        assert_eq!(
            calculate_scope_boost("src/auth/login.rs", &["src/auth".into()]),
            0.8
        );
        assert!((calculate_scope_boost("lib/src/auth/x.rs", &["auth".into()]) - 0.4).abs() < 0.01);
    }
}
