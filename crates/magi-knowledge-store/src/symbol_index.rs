use std::collections::{BTreeSet, HashMap};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::min_heap::MinHeap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Interface,
    Type,
    Enum,
    Variable,
    Method,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: String,
    pub line: usize,
    pub end_line: Option<usize>,
    pub is_exported: bool,
    pub container: Option<String>,
    pub signature: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchType {
    Exact,
    Prefix,
    Contains,
    Fuzzy,
}

#[derive(Clone, Debug)]
pub struct SymbolSearchHit {
    pub symbol: SymbolEntry,
    pub score: f64,
    pub match_type: MatchType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolIndexSnapshot {
    pub symbols: Vec<(String, Vec<SymbolEntry>)>,
    pub file_symbols: Vec<(String, Vec<String>)>,
}

struct SymbolPattern {
    kind: SymbolKind,
    pattern: Regex,
    name_group: usize,
    export_group: Option<usize>,
}

pub struct SymbolIndex {
    symbols: HashMap<String, Vec<SymbolEntry>>,
    file_symbols: HashMap<String, Vec<String>>,
    lang_patterns: HashMap<&'static str, Vec<SymbolPattern>>,
    method_pattern: Regex,
    java_method_pattern: Regex,
    reexport_pattern: Regex,
    ready: bool,
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            file_symbols: HashMap::new(),
            lang_patterns: build_lang_patterns(),
            method_pattern: Regex::new(
                r"^\s+(?:(?:public|private|protected|static|async|readonly|abstract|override)\s+)*([a-zA-Z_$][a-zA-Z0-9_$]*)\s*(?:<[^>]*>)?\s*\(",
            )
            .unwrap(),
            java_method_pattern: Regex::new(
                r"^\s+(?:(?:public|private|protected|static|final|abstract|synchronized|native)\s+)*(?:<[^>]+>\s+)?[A-Za-z_][A-Za-z0-9_<>,\[\]\s.]*\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(",
            )
            .unwrap(),
            reexport_pattern: Regex::new(
                r#"^export\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#,
            )
            .unwrap(),
            ready: false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn build_from_files(&mut self, project_root: &str, files: &[(String, String)]) {
        self.clear();
        for (path, file_type) in files {
            if file_type != "source" && file_type != "test" {
                continue;
            }
            let full_path = format!("{}/{}", project_root, path);
            let Ok(meta) = std::fs::metadata(&full_path) else {
                continue;
            };
            if meta.len() > 500 * 1024 {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                self.extract_symbols(path, &content);
            }
        }
        self.ready = true;
    }

    pub fn search(&self, query: &str, max_results: usize) -> Vec<SymbolSearchHit> {
        if !self.ready || query.trim().is_empty() {
            return Vec::new();
        }
        let query_lower = query.to_ascii_lowercase();
        let mut hits = Vec::new();

        for (name, entries) in &self.symbols {
            let name_lower = name.to_ascii_lowercase();
            let (match_type, base_score) = if name_lower == query_lower {
                (MatchType::Exact, 1.0)
            } else if name_lower.starts_with(&query_lower) {
                (MatchType::Prefix, 0.8)
            } else if name_lower.contains(&query_lower) {
                (MatchType::Contains, 0.5)
            } else if fuzzy_match(&query_lower, &name_lower) {
                (MatchType::Fuzzy, 0.3)
            } else {
                continue;
            };

            for entry in entries {
                let export_bonus = if entry.is_exported { 0.15 } else { 0.0 };
                let kind_weight = kind_weight(entry.kind);
                let score = (base_score + export_bonus + kind_weight * 0.1).min(1.0);
                hits.push(SymbolSearchHit {
                    symbol: entry.clone(),
                    score,
                    match_type,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(max_results);
        hits
    }

    pub fn search_multi(
        &self,
        tokens: &[String],
        max_results: usize,
        original_query: Option<&str>,
    ) -> Vec<SymbolSearchHit> {
        if !self.ready || tokens.is_empty() {
            return Vec::new();
        }

        let mut query_terms = Vec::new();
        let mut seen = BTreeSet::new();
        let mut original_query_lower: Option<String> = None;

        if let Some(oq) = original_query {
            let trimmed = oq.trim();
            if trimmed.len() >= 2 {
                let lower = trimmed.to_ascii_lowercase();
                seen.insert(lower.clone());
                query_terms.push(lower.clone());
                original_query_lower = Some(lower);
            }
        }

        for token in tokens {
            if token.len() < 2 {
                continue;
            }
            let tl = token.to_ascii_lowercase();
            if seen.contains(&tl) {
                continue;
            }
            seen.insert(tl.clone());
            query_terms.push(tl);
        }

        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut hit_map: HashMap<String, SymbolSearchHit> = HashMap::new();

        for (name, entries) in &self.symbols {
            let name_lower = name.to_ascii_lowercase();

            let mut best_match_type: Option<MatchType> = None;
            let mut best_score: f64 = 0.0;

            for term in &query_terms {
                let (mt, sc) = if name_lower == *term {
                    (MatchType::Exact, 1.0)
                } else if name_lower.starts_with(term.as_str()) {
                    (MatchType::Prefix, 0.8)
                } else if name_lower.contains(term.as_str()) {
                    (MatchType::Contains, 0.5)
                } else {
                    continue;
                };

                if sc > best_score {
                    best_score = sc;
                    best_match_type = Some(mt);
                    if sc >= 1.0 {
                        break;
                    }
                }
            }

            if best_match_type.is_none() {
                if let Some(ref oql) = original_query_lower {
                    if fuzzy_match(oql, &name_lower) {
                        best_match_type = Some(MatchType::Fuzzy);
                        best_score = 0.3;
                    }
                }
            }

            if let Some(match_type) = best_match_type {
                for entry in entries {
                    let export_bonus = if entry.is_exported { 0.15 } else { 0.0 };
                    let kw = kind_weight(entry.kind);
                    let final_score = (best_score + export_bonus + kw * 0.1).min(1.0);
                    let key = format!("{}:{}:{}", entry.file_path, name, entry.line);
                    let existing = hit_map.get(&key);
                    if existing.is_none() || existing.map(|e| e.score).unwrap_or(0.0) < final_score
                    {
                        hit_map.insert(
                            key,
                            SymbolSearchHit {
                                symbol: entry.clone(),
                                score: final_score,
                                match_type,
                            },
                        );
                    }
                }
            }
        }

        let cmp = |a: &SymbolSearchHit, b: &SymbolSearchHit| -> std::cmp::Ordering {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        };
        let mut heap = MinHeap::new(max_results, cmp);
        for hit in hit_map.into_values() {
            heap.push(hit);
        }
        heap.into_sorted_desc()
    }

    pub fn get_symbols_for_file(&self, file_path: &str) -> Vec<SymbolEntry> {
        let Some(symbol_names) = self.file_symbols.get(file_path) else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for name in symbol_names {
            if let Some(entries) = self.symbols.get(name) {
                for entry in entries {
                    if entry.file_path == file_path {
                        results.push(entry.clone());
                    }
                }
            }
        }
        results
    }

    pub fn get_vocabulary(&self, max_size: usize) -> BTreeSet<String> {
        let mut vocabulary = BTreeSet::new();
        let valid_word = Regex::new(r"^[a-z0-9_]+$").unwrap();

        for (name, entries) in &self.symbols {
            let word = name.trim().to_ascii_lowercase();
            if word.len() >= 3 && word.len() <= 64 && valid_word.is_match(&word) {
                vocabulary.insert(word);
            }
            for entry in entries {
                if let Some(ref container) = entry.container {
                    let w = container.trim().to_ascii_lowercase();
                    if w.len() >= 3 && w.len() <= 64 && valid_word.is_match(&w) {
                        vocabulary.insert(w);
                    }
                }
                if let Some(ref sig) = entry.signature {
                    for tok in sig.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
                        let w = tok.to_ascii_lowercase();
                        if w.len() >= 3 && w.len() <= 64 && valid_word.is_match(&w) {
                            vocabulary.insert(w);
                        }
                    }
                }
            }
            if vocabulary.len() >= max_size {
                break;
            }
        }
        vocabulary
    }

    pub fn get_symbol_at_line(&self, file_path: &str, line: usize) -> Option<SymbolEntry> {
        let symbols = self.get_symbols_for_file(file_path);
        let mut best_match: Option<SymbolEntry> = None;
        let mut best_range = usize::MAX;

        for sym in symbols {
            let start_line = sym.line;
            let end_line = sym.end_line.unwrap_or(sym.line);
            if line >= start_line && line <= end_line {
                let range = end_line - start_line;
                if range < best_range {
                    best_range = range;
                    best_match = Some(sym);
                }
            }
        }
        best_match
    }

    pub fn update_file(&mut self, project_root: &str, file_path: &str) {
        self.remove_file(file_path);
        let full_path = format!("{}/{}", project_root, file_path);
        let Ok(meta) = std::fs::metadata(&full_path) else {
            return;
        };
        if meta.len() > 500 * 1024 {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            self.extract_symbols(file_path, &content);
        }
    }

    pub fn remove_file(&mut self, file_path: &str) {
        let Some(symbol_names) = self.file_symbols.remove(file_path) else {
            return;
        };
        for name in symbol_names {
            if let Some(entries) = self.symbols.get_mut(&name) {
                entries.retain(|e| e.file_path != file_path);
                if entries.is_empty() {
                    self.symbols.remove(&name);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.symbols.clear();
        self.file_symbols.clear();
        self.ready = false;
    }

    pub fn get_stats(&self) -> SymbolIndexStats {
        let mut total_entries = 0;
        for entries in self.symbols.values() {
            total_entries += entries.len();
        }
        SymbolIndexStats {
            unique_symbols: self.symbols.len(),
            total_entries,
            is_ready: self.ready,
        }
    }

    pub fn to_snapshot(&self) -> SymbolIndexSnapshot {
        SymbolIndexSnapshot {
            symbols: self
                .symbols
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            file_symbols: self
                .file_symbols
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    pub fn from_snapshot(&mut self, snapshot: SymbolIndexSnapshot) {
        self.clear();
        for (name, mut entries) in snapshot.symbols {
            for entry in &mut entries {
                if entry.end_line.is_none() {
                    entry.end_line = Some(entry.line);
                }
            }
            self.symbols.insert(name, entries);
        }
        self.file_symbols = snapshot.file_symbols.into_iter().collect();
        self.ready = true;
    }

    fn extract_symbols(&mut self, file_path: &str, content: &str) {
        let ext = file_extension(file_path);

        // 优先用 tree-sitter（AST 级）提取；支持的语言走语法树，
        // 不支持的语言回落到下方逐行正则提取（单一职责，按语言路由不双轨）。
        if crate::ts_symbol_extract::tree_sitter_supports(ext) {
            if let Some(entries) = crate::ts_symbol_extract::extract_symbols(file_path, content, ext)
            {
                let mut symbol_names = Vec::with_capacity(entries.len());
                for entry in entries {
                    symbol_names.push(entry.name.clone());
                    self.symbols.entry(entry.name.clone()).or_default().push(entry);
                }
                if !symbol_names.is_empty() {
                    self.file_symbols.insert(file_path.to_string(), symbol_names);
                }
                return;
            }
        }

        let Some(patterns) = self.lang_patterns.get(ext) else {
            return;
        };

        let is_ts = matches!(ext, ".ts" | ".tsx" | ".js" | ".jsx" | ".mjs" | ".cjs");
        let is_java = ext == ".java";
        let is_csharp = ext == ".cs";
        let is_cpp = matches!(ext, ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh");
        let has_class_scope = is_ts || is_java || is_csharp || is_cpp;

        let lines: Vec<&str> = content.lines().collect();
        let mut symbol_names: Vec<String> = Vec::new();

        let mut brace_depth: i32 = 0;
        let mut class_stack: Vec<(String, i32)> = Vec::new();
        let mut pending_blocks: Vec<(SymbolEntry, i32)> = Vec::new();
        let mut pending_declaration: Option<(SymbolEntry, usize)> = None;
        let mut in_multi_line_string: Option<char> = None;
        let mut in_block_comment = false;

        for (line_idx, line) in lines.iter().enumerate() {
            if in_block_comment {
                if line.contains("*/") {
                    in_block_comment = false;
                }
                continue;
            }

            if let Some(quote_char) = in_multi_line_string {
                let unescaped = line
                    .replace("\\\\", "__")
                    .replace("\\'", "__")
                    .replace("\\\"", "__")
                    .replace("\\`", "__");
                if unescaped.contains(quote_char) {
                    in_multi_line_string = None;
                }
                continue;
            }

            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with("/*") {
                if !trimmed.contains("*/") {
                    in_block_comment = true;
                }
                continue;
            }

            // re-export (TS/JS)
            if is_ts {
                if let Some(caps) = self.reexport_pattern.captures(line) {
                    if let Some(exported_names_str) = caps.get(1) {
                        for name_part in exported_names_str.as_str().split(',') {
                            let name = name_part.trim().split(" as ").last().unwrap_or("").trim();
                            if name.len() >= 2 {
                                let entry = SymbolEntry {
                                    name: name.to_string(),
                                    kind: SymbolKind::Variable,
                                    file_path: file_path.to_string(),
                                    line: line_idx,
                                    end_line: Some(line_idx),
                                    is_exported: true,
                                    container: None,
                                    signature: None,
                                };
                                self.symbols
                                    .entry(name.to_string())
                                    .or_default()
                                    .push(entry);
                                symbol_names.push(name.to_string());
                            }
                        }
                    }
                }
            }

            let (stripped_line, opens_multi) = strip_strings(line);
            if let Some(q) = opens_multi {
                in_multi_line_string = Some(q);
            }
            let open_braces = stripped_line.matches('{').count() as i32;
            let close_braces = stripped_line.matches('}').count() as i32;

            if has_class_scope {
                for sp in patterns.iter() {
                    if sp.kind == SymbolKind::Class {
                        if let Some(caps) = sp.pattern.captures(line) {
                            if let Some(m) = caps.get(sp.name_group) {
                                let class_name = m.as_str().to_string();
                                class_stack.push((class_name, brace_depth));
                            }
                        }
                    }
                }
            }

            let prev_brace_depth = brace_depth;
            brace_depth += open_braces - close_braces;

            if let Some((ref decl_entry, max_line)) = pending_declaration.take() {
                if line_idx > max_line {
                    let mut entry = decl_entry.clone();
                    entry.end_line = Some(entry.line);
                    // already added to symbols earlier, update end_line in-place
                    update_end_line(
                        &mut self.symbols,
                        &entry.name,
                        &entry.file_path,
                        entry.line,
                        entry.end_line,
                    );
                } else if open_braces > 0 {
                    pending_blocks.push((decl_entry.clone(), prev_brace_depth));
                } else {
                    pending_declaration = Some((decl_entry.clone(), max_line));
                }
            }

            while !class_stack.is_empty() && brace_depth <= class_stack.last().unwrap().1 {
                class_stack.pop();
            }

            while !pending_blocks.is_empty() && brace_depth <= pending_blocks.last().unwrap().1 {
                let (mut closed_entry, _) = pending_blocks.pop().unwrap();
                closed_entry.end_line = Some(line_idx);
                update_end_line(
                    &mut self.symbols,
                    &closed_entry.name,
                    &closed_entry.file_path,
                    closed_entry.line,
                    closed_entry.end_line,
                );
            }

            let current_class = class_stack.last().map(|(name, _)| name.clone());

            for sp in patterns.iter() {
                if let Some(caps) = sp.pattern.captures(line) {
                    let Some(name_match) = caps.get(sp.name_group) else {
                        continue;
                    };
                    let name = name_match.as_str();
                    if name.len() < 2 {
                        continue;
                    }

                    let is_exported = if let Some(eg) = sp.export_group {
                        caps.get(eg)
                            .map(|m| m.as_str().contains("export"))
                            .unwrap_or(false)
                    } else {
                        name.starts_with(|c: char| c.is_ascii_uppercase())
                    };

                    let is_block_kind = matches!(
                        sp.kind,
                        SymbolKind::Class
                            | SymbolKind::Interface
                            | SymbolKind::Enum
                            | SymbolKind::Function
                    );
                    let has_block = open_braces > 0 && is_block_kind;

                    let entry = SymbolEntry {
                        name: name.to_string(),
                        kind: sp.kind,
                        file_path: file_path.to_string(),
                        line: line_idx,
                        end_line: if is_block_kind { None } else { Some(line_idx) },
                        is_exported,
                        container: None,
                        signature: None,
                    };

                    if has_block {
                        pending_blocks.push((entry.clone(), prev_brace_depth));
                    } else if is_block_kind {
                        if let Some((ref old_entry, _)) = pending_declaration {
                            update_end_line(
                                &mut self.symbols,
                                &old_entry.name,
                                &old_entry.file_path,
                                old_entry.line,
                                Some(old_entry.line),
                            );
                        }
                        pending_declaration = Some((entry.clone(), line_idx + 10));
                    }

                    self.symbols
                        .entry(name.to_string())
                        .or_default()
                        .push(entry);
                    symbol_names.push(name.to_string());
                }
            }

            // method extraction (TS/JS, Java, C#, C++)
            if has_class_scope && current_class.is_some() && !trimmed.is_empty() {
                let method_pat = if is_java || is_csharp {
                    &self.java_method_pattern
                } else {
                    &self.method_pattern
                };
                if let Some(caps) = method_pat.captures(line) {
                    if let Some(m) = caps.get(1) {
                        let method_name = m.as_str();
                        let class_name = current_class.as_ref().unwrap();
                        if method_name.len() >= 2
                            && method_name != "constructor"
                            && !((is_java || is_csharp) && method_name == class_name.as_str())
                        {
                            let has_method_block = open_braces > 0;
                            let is_method_exported = if is_java || is_csharp {
                                trimmed.starts_with("public ")
                            } else {
                                false
                            };

                            let entry = SymbolEntry {
                                name: method_name.to_string(),
                                kind: SymbolKind::Method,
                                file_path: file_path.to_string(),
                                line: line_idx,
                                end_line: None,
                                is_exported: is_method_exported,
                                container: Some(class_name.clone()),
                                signature: None,
                            };

                            self.symbols
                                .entry(method_name.to_string())
                                .or_default()
                                .push(entry.clone());
                            symbol_names.push(method_name.to_string());

                            if has_method_block {
                                pending_blocks.push((entry, prev_brace_depth));
                            } else {
                                if let Some((ref old_entry, _)) = pending_declaration {
                                    update_end_line(
                                        &mut self.symbols,
                                        &old_entry.name,
                                        &old_entry.file_path,
                                        old_entry.line,
                                        Some(old_entry.line),
                                    );
                                }
                                pending_declaration = Some((entry, line_idx + 10));
                            }
                        }
                    }
                }
            }
        }

        if let Some((ref entry, _)) = pending_declaration {
            update_end_line(
                &mut self.symbols,
                &entry.name,
                &entry.file_path,
                entry.line,
                Some(entry.line),
            );
        }
        for (entry, _) in &pending_blocks {
            if entry.end_line.is_none() {
                update_end_line(
                    &mut self.symbols,
                    &entry.name,
                    &entry.file_path,
                    entry.line,
                    Some(lines.len().saturating_sub(1)),
                );
            }
        }

        if !symbol_names.is_empty() {
            self.file_symbols
                .insert(file_path.to_string(), symbol_names);
        }
    }
}

#[derive(Clone, Debug)]
pub struct SymbolIndexStats {
    pub unique_symbols: usize,
    pub total_entries: usize,
    pub is_ready: bool,
}

fn fuzzy_match(query: &str, target: &str) -> bool {
    if query.len() < 3 {
        return false;
    }
    let mut qi = 0;
    let query_bytes = query.as_bytes();
    for &tb in target.as_bytes() {
        if qi < query_bytes.len() && query_bytes[qi] == tb {
            qi += 1;
        }
    }
    qi == query_bytes.len()
}

fn kind_weight(kind: SymbolKind) -> f64 {
    match kind {
        SymbolKind::Class => 1.0,
        SymbolKind::Interface => 0.9,
        SymbolKind::Function => 0.8,
        SymbolKind::Type => 0.7,
        SymbolKind::Enum => 0.7,
        SymbolKind::Method => 0.6,
        SymbolKind::Variable => 0.5,
    }
}

fn file_extension(path: &str) -> &str {
    if let Some(pos) = path.rfind('.') {
        &path[pos..]
    } else {
        ""
    }
}

fn strip_strings(line: &str) -> (String, Option<char>) {
    let chars: Vec<char> = line.chars().collect();
    let mut result = Vec::with_capacity(chars.len());
    let mut i = 0;
    let mut opens_multi_line = None;

    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            result.push('"');
            result.push('"');
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    closed = true;
                    i += 1;
                    break;
                }
                i += 1;
            }
            if !closed {
                opens_multi_line = Some(quote);
            }
        } else {
            result.push(c);
            i += 1;
        }
    }

    (result.into_iter().collect(), opens_multi_line)
}

fn update_end_line(
    symbols: &mut HashMap<String, Vec<SymbolEntry>>,
    name: &str,
    file_path: &str,
    line: usize,
    end_line: Option<usize>,
) {
    if let Some(entries) = symbols.get_mut(name) {
        for entry in entries.iter_mut() {
            if entry.file_path == file_path && entry.line == line {
                entry.end_line = end_line;
                return;
            }
        }
    }
}

fn build_lang_patterns() -> HashMap<&'static str, Vec<SymbolPattern>> {
    let mut map = HashMap::new();

    let ts_patterns = vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(export\s+)?(?:async\s+)?function\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(export\s+)?(?:const|let|var)\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s*=\s*(?:async\s+)?(?:\([^)]*\)|[a-zA-Z_$][a-zA-Z0-9_$]*)\s*=>").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(export\s+)?(?:abstract\s+)?class\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(export\s+)?interface\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Type,
            pattern: Regex::new(r"^(export\s+)?type\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(export\s+)?(?:const\s+)?enum\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
        SymbolPattern {
            kind: SymbolKind::Variable,
            pattern: Regex::new(r"^(export\s+)(?:const|let|var)\s+([A-Z][a-zA-Z0-9_$]*)").unwrap(),
            name_group: 2,
            export_group: Some(1),
        },
    ];
    for ext in [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"] {
        map.insert(ext, Vec::new());
    }
    for ext in [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"] {
        map.insert(
            ext,
            ts_patterns
                .iter()
                .map(|sp| SymbolPattern {
                    kind: sp.kind,
                    pattern: sp.pattern.clone(),
                    name_group: sp.name_group,
                    export_group: sp.export_group,
                })
                .collect(),
        );
    }

    map.insert(
        ".py",
        vec![
            SymbolPattern {
                kind: SymbolKind::Function,
                pattern: Regex::new(r"^(?:async\s+)?def\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
            SymbolPattern {
                kind: SymbolKind::Class,
                pattern: Regex::new(r"^class\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
        ],
    );

    map.insert(
        ".go",
        vec![
            SymbolPattern {
                kind: SymbolKind::Function,
                pattern: Regex::new(r"^func\s+(?:\([^)]+\)\s+)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
            SymbolPattern {
                kind: SymbolKind::Type,
                pattern: Regex::new(r"^type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface)")
                    .unwrap(),
                name_group: 1,
                export_group: None,
            },
        ],
    );

    map.insert(".java", vec![
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+)?(?:abstract\s+|final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ]);

    let rust_patterns = vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(
                r"^(?:pub\s+(?:\(crate\)\s+)?)?(?:async\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)",
            )
            .unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:pub\s+(?:\(crate\)\s+)?)?struct\s+([A-Za-z_][A-Za-z0-9_]*)")
                .unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(?:pub\s+(?:\(crate\)\s+)?)?trait\s+([A-Za-z_][A-Za-z0-9_]*)")
                .unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(?:pub\s+(?:\(crate\)\s+)?)?enum\s+([A-Za-z_][A-Za-z0-9_]*)")
                .unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Type,
            pattern: Regex::new(r"^(?:pub\s+(?:\(crate\)\s+)?)?type\s+([A-Za-z_][A-Za-z0-9_]*)")
                .unwrap(),
            name_group: 1,
            export_group: None,
        },
    ];
    map.insert(".rs", rust_patterns);

    let c_patterns = vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(?:static\s+|inline\s+|extern\s+|virtual\s+)*(?:const\s+)?[A-Za-z_][A-Za-z0-9_*&\s:<>,]*\s+\*?([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:typedef\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Type,
            pattern: Regex::new(r"^typedef\s+.*\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Type,
            pattern: Regex::new(r"^namespace\s+([A-Za-z_][A-Za-z0-9_:]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^enum\s+(?:class\s+)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ];
    for ext in [".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh"] {
        map.insert(
            ext,
            c_patterns
                .iter()
                .map(|sp| SymbolPattern {
                    kind: sp.kind,
                    pattern: sp.pattern.clone(),
                    name_group: sp.name_group,
                    export_group: sp.export_group,
                })
                .collect(),
        );
    }

    map.insert(".cs", vec![
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:abstract\s+|sealed\s+|static\s+|partial\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:partial\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:readonly\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Type,
            pattern: Regex::new(r"^namespace\s+([A-Za-z_][A-Za-z0-9_.]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ]);

    map.insert(".php", vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+)?(?:static\s+)?function\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:abstract\s+|final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^trait\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ]);

    map.insert(
        ".rb",
        vec![
            SymbolPattern {
                kind: SymbolKind::Function,
                pattern: Regex::new(r"^\s*def\s+(?:self\.)?([a-zA-Z_][a-zA-Z0-9_!?=]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
            SymbolPattern {
                kind: SymbolKind::Class,
                pattern: Regex::new(r"^\s*class\s+([A-Z][A-Za-z0-9_]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
            SymbolPattern {
                kind: SymbolKind::Type,
                pattern: Regex::new(r"^\s*module\s+([A-Z][A-Za-z0-9_]*)").unwrap(),
                name_group: 1,
                export_group: None,
            },
        ],
    );

    map.insert(".swift", vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?(?:static\s+|class\s+)?func\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?(?:final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?protocol\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(?:public\s+|open\s+|private\s+|internal\s+|fileprivate\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ]);

    let kotlin_patterns = vec![
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:suspend\s+|inline\s+|override\s+)*fun\s+(?:<[^>]+>\s+)?([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:data\s+|sealed\s+|abstract\s+|open\s+|inner\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?(?:sealed\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?object\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Enum,
            pattern: Regex::new(r"^(?:public\s+|private\s+|protected\s+|internal\s+)?enum\s+class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ];
    for ext in [".kt", ".kts"] {
        map.insert(
            ext,
            kotlin_patterns
                .iter()
                .map(|sp| SymbolPattern {
                    kind: sp.kind,
                    pattern: sp.pattern.clone(),
                    name_group: sp.name_group,
                    export_group: sp.export_group,
                })
                .collect(),
        );
    }

    let objc_patterns = vec![
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^@interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Class,
            pattern: Regex::new(r"^@implementation\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Interface,
            pattern: Regex::new(r"^@protocol\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Method,
            pattern: Regex::new(r"^[-+]\s*\([^)]+\)\s*([a-zA-Z_][a-zA-Z0-9_]*)").unwrap(),
            name_group: 1,
            export_group: None,
        },
        SymbolPattern {
            kind: SymbolKind::Function,
            pattern: Regex::new(r"^(?:static\s+|inline\s+|extern\s+)*[A-Za-z_][A-Za-z0-9_*\s]+\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap(),
            name_group: 1,
            export_group: None,
        },
    ];
    for ext in [".m", ".mm"] {
        map.insert(
            ext,
            objc_patterns
                .iter()
                .map(|sp| SymbolPattern {
                    kind: sp.kind,
                    pattern: sp.pattern.clone(),
                    name_group: sp.name_group,
                    export_group: sp.export_group,
                })
                .collect(),
        );
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match() {
        assert!(fuzzy_match("gpc", "getprojectcontext"));
        assert!(fuzzy_match("idx", "invertedindex"));
        assert!(!fuzzy_match("xyz", "getprojectcontext"));
        assert!(!fuzzy_match("ab", "abc")); // too short
    }

    #[test]
    fn test_strip_strings() {
        let (stripped, multi) = strip_strings(r#"const x = "hello world" + y"#);
        assert!(!stripped.contains("hello"));
        assert!(multi.is_none());

        let (_stripped2, multi2) = strip_strings(r#"const x = "unclosed"#);
        assert!(multi2.is_some());
    }

    #[test]
    fn test_extract_ts_symbols() {
        let mut idx = SymbolIndex::new();
        let content = r#"export function getProjectContext(root: string): ProjectContext {
  return { root };
}

export class ProjectManager {
  private root: string;

  constructor(root: string) {
    this.root = root;
  }

  getRoot(): string {
    return this.root;
  }

  async findFiles(pattern: string): Promise<string[]> {
    return [];
  }
}

export interface IProjectConfig {
  root: string;
  name: string;
}

export type ProjectId = string;

export enum ProjectStatus {
  Active,
  Archived,
}

export const MAX_FILES = 1000;
"#;
        idx.extract_symbols("src/project.ts", content);
        let symbols = idx.get_symbols_for_file("src/project.ts");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"getProjectContext"));
        assert!(names.contains(&"ProjectManager"));
        assert!(names.contains(&"IProjectConfig"));
        assert!(names.contains(&"ProjectId"));
        assert!(names.contains(&"ProjectStatus"));
        assert!(names.contains(&"MAX_FILES"));

        let pm = symbols.iter().find(|s| s.name == "ProjectManager").unwrap();
        assert_eq!(pm.kind, SymbolKind::Class);
        assert!(pm.is_exported);

        let methods: Vec<&SymbolEntry> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        let method_names: Vec<&str> = methods.iter().map(|s| s.name.as_str()).collect();
        assert!(method_names.contains(&"getRoot"));
        assert!(method_names.contains(&"findFiles"));

        for method in &methods {
            assert_eq!(method.container.as_deref(), Some("ProjectManager"));
        }
    }

    #[test]
    fn test_extract_rust_symbols() {
        let mut idx = SymbolIndex::new();
        let content = r#"pub struct InvertedIndex {
    postings: HashMap<String, HashMap<String, PostingEntry>>,
}

pub trait Searchable {
    fn search(&self, query: &str) -> Vec<Hit>;
}

pub enum IndexState {
    Building,
    Ready,
}

pub type IndexResult = Result<(), Error>;

pub fn create_index() -> InvertedIndex {
    InvertedIndex { postings: HashMap::new() }
}
"#;
        idx.extract_symbols("src/index.rs", content);
        let symbols = idx.get_symbols_for_file("src/index.rs");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"InvertedIndex"));
        assert!(names.contains(&"Searchable"));
        assert!(names.contains(&"IndexState"));
        assert!(names.contains(&"IndexResult"));
        assert!(names.contains(&"create_index"));
    }

    #[test]
    fn test_extract_python_symbols() {
        let mut idx = SymbolIndex::new();
        let content = r#"class UserService:
    def __init__(self, db):
        self.db = db

    async def get_user(self, user_id):
        pass

def create_app():
    pass
"#;
        idx.extract_symbols("app/services.py", content);
        let symbols = idx.get_symbols_for_file("app/services.py");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"create_app"));
    }

    #[test]
    fn test_extract_go_symbols() {
        let mut idx = SymbolIndex::new();
        let content = r#"func NewServer(config Config) *Server {
    return &Server{}
}

func (s *Server) HandleRequest(w http.ResponseWriter, r *http.Request) {
}

type Config struct {
    Port int
}

type Handler interface {
    Handle(ctx context.Context) error
}
"#;
        idx.extract_symbols("server/main.go", content);
        let symbols = idx.get_symbols_for_file("server/main.go");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"NewServer"));
        assert!(names.contains(&"HandleRequest"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Handler"));
    }

    #[test]
    fn test_search_exact_prefix_contains_fuzzy() {
        let mut idx = SymbolIndex::new();
        idx.symbols.insert(
            "getProjectContext".to_string(),
            vec![SymbolEntry {
                name: "getProjectContext".to_string(),
                kind: SymbolKind::Function,
                file_path: "src/project.ts".to_string(),
                line: 0,
                end_line: Some(2),
                is_exported: true,
                container: None,
                signature: None,
            }],
        );
        idx.ready = true;

        let exact = idx.search("getProjectContext", 10);
        assert_eq!(exact[0].match_type, MatchType::Exact);

        let prefix = idx.search("getProject", 10);
        assert_eq!(prefix[0].match_type, MatchType::Prefix);

        let contains = idx.search("Project", 10);
        assert_eq!(contains[0].match_type, MatchType::Contains);

        let fuzzy = idx.search("gpc", 10);
        assert_eq!(fuzzy[0].match_type, MatchType::Fuzzy);
    }

    #[test]
    fn test_search_multi() {
        let mut idx = SymbolIndex::new();
        idx.symbols.insert(
            "getProjectContext".to_string(),
            vec![SymbolEntry {
                name: "getProjectContext".to_string(),
                kind: SymbolKind::Function,
                file_path: "src/a.ts".to_string(),
                line: 0,
                end_line: Some(5),
                is_exported: true,
                container: None,
                signature: None,
            }],
        );
        idx.symbols.insert(
            "ProjectManager".to_string(),
            vec![SymbolEntry {
                name: "ProjectManager".to_string(),
                kind: SymbolKind::Class,
                file_path: "src/b.ts".to_string(),
                line: 0,
                end_line: Some(20),
                is_exported: true,
                container: None,
                signature: None,
            }],
        );
        idx.ready = true;

        let hits = idx.search_multi(
            &["project".to_string(), "context".to_string()],
            10,
            Some("project context"),
        );
        assert!(!hits.is_empty());
    }

    #[test]
    fn test_get_symbol_at_line() {
        let mut idx = SymbolIndex::new();
        idx.symbols.insert(
            "MyClass".to_string(),
            vec![SymbolEntry {
                name: "MyClass".to_string(),
                kind: SymbolKind::Class,
                file_path: "src/a.ts".to_string(),
                line: 0,
                end_line: Some(20),
                is_exported: true,
                container: None,
                signature: None,
            }],
        );
        idx.symbols.insert(
            "myMethod".to_string(),
            vec![SymbolEntry {
                name: "myMethod".to_string(),
                kind: SymbolKind::Method,
                file_path: "src/a.ts".to_string(),
                line: 5,
                end_line: Some(10),
                is_exported: false,
                container: Some("MyClass".to_string()),
                signature: None,
            }],
        );
        idx.file_symbols.insert(
            "src/a.ts".to_string(),
            vec!["MyClass".to_string(), "myMethod".to_string()],
        );

        let sym = idx.get_symbol_at_line("src/a.ts", 7).unwrap();
        assert_eq!(sym.name, "myMethod");

        let sym2 = idx.get_symbol_at_line("src/a.ts", 15).unwrap();
        assert_eq!(sym2.name, "MyClass");
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let mut idx = SymbolIndex::new();
        idx.symbols.insert(
            "TestFunc".to_string(),
            vec![SymbolEntry {
                name: "TestFunc".to_string(),
                kind: SymbolKind::Function,
                file_path: "src/test.go".to_string(),
                line: 0,
                end_line: Some(5),
                is_exported: true,
                container: None,
                signature: None,
            }],
        );
        idx.file_symbols
            .insert("src/test.go".to_string(), vec!["TestFunc".to_string()]);
        idx.ready = true;

        let snapshot = idx.to_snapshot();
        let mut idx2 = SymbolIndex::new();
        idx2.from_snapshot(snapshot);

        assert!(idx2.is_ready());
        let symbols = idx2.get_symbols_for_file("src/test.go");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "TestFunc");
    }

    #[test]
    fn test_reexport_detection() {
        let mut idx = SymbolIndex::new();
        let content = r#"export { logger, LogCategory } from './logging';
export { createApp as default } from './app';
"#;
        idx.extract_symbols("src/index.ts", content);
        let symbols = idx.get_symbols_for_file("src/index.ts");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"logger"));
        assert!(names.contains(&"LogCategory"));
        assert!(names.contains(&"default"));
    }

    #[test]
    fn test_kind_weight_ordering() {
        assert!(kind_weight(SymbolKind::Class) > kind_weight(SymbolKind::Interface));
        assert!(kind_weight(SymbolKind::Interface) > kind_weight(SymbolKind::Function));
        assert!(kind_weight(SymbolKind::Function) > kind_weight(SymbolKind::Method));
        assert!(kind_weight(SymbolKind::Method) > kind_weight(SymbolKind::Variable));
    }
}
