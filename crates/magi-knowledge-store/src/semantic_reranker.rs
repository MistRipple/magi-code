use std::collections::HashMap;

use crate::result_ranker::RankedResult;
use crate::symbol_index::SymbolIndex;

const CACHE_MAX: usize = 30;
const CACHE_TTL_MS: u64 = 120_000;

#[derive(Clone, Debug)]
struct RerankerCacheEntry {
    reordered_paths: Vec<String>,
    timestamp: u64,
}

pub struct SemanticReranker {
    cache: HashMap<String, RerankerCacheEntry>,
}

impl Default for SemanticReranker {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticReranker {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn build_prompt(
        query: &str,
        candidates: &[RankedResult],
        symbol_index: Option<&SymbolIndex>,
    ) -> String {
        let summary = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let symbols = get_top_symbols(&c.file_path, 5, symbol_index);
                let sym_str = if symbols.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", symbols.join(", "))
                };
                format!("{}. {}{}", i + 1, c.file_path, sym_str)
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"你是代码搜索结果排序器。根据用户查询意图，对候选文件重新排序。

查询: "{}"

候选文件:
{}

输出: 按相关性从高到低排列的文件序号（逗号分隔，如: 3,1,5,2,4）。只输出序号，无其他文字。"#,
            query, summary
        )
    }

    pub fn rerank(
        &mut self,
        query: &str,
        candidates: Vec<RankedResult>,
        llm_response: Option<&str>,
        _symbol_index: Option<&SymbolIndex>,
        top_n: usize,
    ) -> Vec<RankedResult> {
        if candidates.len() <= 2 {
            return candidates;
        }

        let split_at = top_n.min(candidates.len());
        let rerank_slice: Vec<RankedResult> = candidates[..split_at].to_vec();
        let rest_slice: Vec<RankedResult> = candidates[split_at..].to_vec();

        let cache_key = build_cache_key(query, &rerank_slice);
        let now = now_millis();
        if let Some(cached) = self.cache.get(&cache_key) {
            if now - cached.timestamp < CACHE_TTL_MS {
                return apply_reorder(&rerank_slice, &rest_slice, &cached.reordered_paths);
            }
        }

        let response = match llm_response {
            Some(r) => r,
            None => {
                return [rerank_slice, rest_slice].concat();
            }
        };

        let reordered_paths = parse_reorder_response(response, &rerank_slice);
        if reordered_paths.is_empty() {
            return [rerank_slice, rest_slice].concat();
        }

        self.cache_set(cache_key, reordered_paths.clone());

        apply_reorder(&rerank_slice, &rest_slice, &reordered_paths)
    }

    pub fn build_rerank_prompt(
        &self,
        query: &str,
        candidates: &[RankedResult],
        symbol_index: Option<&SymbolIndex>,
        top_n: usize,
    ) -> Option<String> {
        if candidates.len() <= 2 {
            return None;
        }
        let split_at = top_n.min(candidates.len());
        let rerank_slice = &candidates[..split_at];

        let cache_key = build_cache_key(query, rerank_slice);
        let now = now_millis();
        if let Some(cached) = self.cache.get(&cache_key) {
            if now - cached.timestamp < CACHE_TTL_MS {
                return None;
            }
        }

        Some(Self::build_prompt(query, rerank_slice, symbol_index))
    }

    fn cache_set(&mut self, key: String, paths: Vec<String>) {
        if self.cache.contains_key(&key) {
            self.cache.remove(&key);
        }
        while self.cache.len() >= CACHE_MAX {
            if let Some(first_key) = self.cache.keys().next().cloned() {
                self.cache.remove(&first_key);
            } else {
                break;
            }
        }
        self.cache.insert(
            key,
            RerankerCacheEntry {
                reordered_paths: paths,
                timestamp: now_millis(),
            },
        );
    }
}

fn get_top_symbols(file_path: &str, max: usize, symbol_index: Option<&SymbolIndex>) -> Vec<String> {
    let idx = match symbol_index {
        Some(idx) if idx.is_ready() => idx,
        _ => return Vec::new(),
    };
    let mut symbols = idx.get_symbols_for_file(file_path);
    if symbols.is_empty() {
        return Vec::new();
    }
    symbols.sort_by(|a, b| b.is_exported.cmp(&a.is_exported));
    symbols.truncate(max);
    symbols.iter().map(|s| s.name.clone()).collect()
}

fn parse_reorder_response(response: &str, candidates: &[RankedResult]) -> Vec<String> {
    let nums: Vec<usize> = response
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<usize>().ok())
        .collect();

    if nums.is_empty() {
        return Vec::new();
    }

    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();

    for n in &nums {
        let idx = n.wrapping_sub(1);
        if idx < candidates.len() && seen.insert(idx) {
            paths.push(candidates[idx].file_path.clone());
        }
    }

    if paths.len() < candidates.len() / 2 {
        return Vec::new();
    }

    for (i, c) in candidates.iter().enumerate() {
        if !seen.contains(&i) {
            paths.push(c.file_path.clone());
        }
    }

    paths
}

fn apply_reorder(
    slice: &[RankedResult],
    rest: &[RankedResult],
    paths: &[String],
) -> Vec<RankedResult> {
    let map: HashMap<String, &RankedResult> =
        slice.iter().map(|c| (c.file_path.clone(), c)).collect();

    let max_score = slice.first().map(|c| c.final_score).unwrap_or(1.0);

    let mut reordered: Vec<RankedResult> = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        if let Some(c) = map.get(path) {
            let mut result = (*c).clone();
            result.final_score = max_score * (1.0 - i as f64 * 0.02);
            reordered.push(result);
        }
    }

    reordered.extend_from_slice(rest);
    reordered
}

fn build_cache_key(query: &str, candidates: &[RankedResult]) -> String {
    let paths: Vec<&str> = candidates.iter().map(|c| c.file_path.as_str()).collect();
    format!("{}::{}", query.trim().to_lowercase(), paths.join("|"))
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
    use crate::result_ranker::ScoreDimensions;

    fn make_candidate(path: &str, score: f64) -> RankedResult {
        RankedResult {
            file_path: path.to_string(),
            final_score: score,
            breakdown: ScoreDimensions {
                tfidf: 0.0,
                symbol_match: 0.0,
                position_weight: 0.0,
                centrality: 0.0,
                recency: 0.0,
                type_weight: 0.0,
                recent_edit_boost: 0.0,
                scope_boost: 0.0,
            },
            sources: vec!["index".to_string()],
        }
    }

    #[test]
    fn test_parse_reorder_response() {
        let candidates = vec![
            make_candidate("a.rs", 3.0),
            make_candidate("b.rs", 2.0),
            make_candidate("c.rs", 1.0),
        ];
        let paths = parse_reorder_response("3,1,2", &candidates);
        assert_eq!(paths, vec!["c.rs", "a.rs", "b.rs"]);
    }

    #[test]
    fn test_parse_reorder_insufficient() {
        let candidates = vec![
            make_candidate("a.rs", 3.0),
            make_candidate("b.rs", 2.0),
            make_candidate("c.rs", 1.0),
            make_candidate("d.rs", 0.5),
        ];
        let paths = parse_reorder_response("1", &candidates);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_rerank_passthrough_small() {
        let mut reranker = SemanticReranker::new();
        let candidates = vec![make_candidate("a.rs", 1.0), make_candidate("b.rs", 0.5)];
        let result = reranker.rerank("test", candidates.clone(), Some("2,1"), None, 15);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].file_path, "a.rs");
    }

    #[test]
    fn test_rerank_with_response() {
        let mut reranker = SemanticReranker::new();
        let candidates = vec![
            make_candidate("a.rs", 3.0),
            make_candidate("b.rs", 2.0),
            make_candidate("c.rs", 1.0),
        ];
        let result = reranker.rerank("test", candidates, Some("3,1,2"), None, 15);
        assert_eq!(result[0].file_path, "c.rs");
        assert_eq!(result[1].file_path, "a.rs");
        assert_eq!(result[2].file_path, "b.rs");
    }

    #[test]
    fn test_cache_hit() {
        let mut reranker = SemanticReranker::new();
        let candidates = vec![
            make_candidate("a.rs", 3.0),
            make_candidate("b.rs", 2.0),
            make_candidate("c.rs", 1.0),
        ];
        reranker.rerank("test", candidates.clone(), Some("3,1,2"), None, 15);
        let result = reranker.rerank("test", candidates, None, None, 15);
        assert_eq!(result[0].file_path, "c.rs");
    }

    #[test]
    fn test_build_prompt() {
        let candidates = vec![
            make_candidate("src/auth.rs", 3.0),
            make_candidate("src/login.rs", 2.0),
        ];
        let prompt = SemanticReranker::build_prompt("登录逻辑", &candidates, None);
        assert!(prompt.contains("1. src/auth.rs"));
        assert!(prompt.contains("2. src/login.rs"));
        assert!(prompt.contains("登录逻辑"));
    }
}
