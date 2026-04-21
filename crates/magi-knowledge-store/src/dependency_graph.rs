use std::collections::{HashMap, HashSet, VecDeque};

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportType {
    Static,
    Dynamic,
    Require,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub import_type: ImportType,
}

#[derive(Clone, Debug)]
pub struct FileCentrality {
    pub file_path: String,
    pub in_degree: usize,
    pub out_degree: usize,
    pub centrality: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyGraphSnapshot {
    pub forward_deps: Vec<(String, Vec<String>)>,
    pub reverse_deps: Vec<(String, Vec<String>)>,
    pub edges: Vec<DependencyEdge>,
    pub centrality_cache: Vec<(String, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpandDirection {
    Forward,
    Reverse,
    Both,
}

struct ImportPatterns {
    es_import: Vec<Regex>,
    reexport: Vec<Regex>,
    require: Regex,
    dynamic_import: Regex,
    py_import: Vec<Regex>,
}

pub struct DependencyGraph {
    forward_deps: HashMap<String, HashSet<String>>,
    reverse_deps: HashMap<String, HashSet<String>>,
    edges: Vec<DependencyEdge>,
    centrality_cache: HashMap<String, f64>,
    file_set: HashSet<String>,
    project_root: String,
    path_aliases: HashMap<String, String>,
    patterns: ImportPatterns,
    ready: bool,
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            forward_deps: HashMap::new(),
            reverse_deps: HashMap::new(),
            edges: Vec::new(),
            centrality_cache: HashMap::new(),
            file_set: HashSet::new(),
            project_root: String::new(),
            path_aliases: HashMap::new(),
            patterns: ImportPatterns {
                es_import: vec![
                    Regex::new(r#"^import\s+\{[^}]+\}\s+from\s+['"]([^'"]+)['"]"#).unwrap(),
                    Regex::new(r#"^import\s+[a-zA-Z_$][a-zA-Z0-9_$]*\s+from\s+['"]([^'"]+)['"]"#).unwrap(),
                    Regex::new(r#"^import\s+\*\s+as\s+[a-zA-Z_$][a-zA-Z0-9_$]*\s+from\s+['"]([^'"]+)['"]"#).unwrap(),
                    Regex::new(r#"^import\s+['"]([^'"]+)['"]"#).unwrap(),
                ],
                reexport: vec![
                    Regex::new(r#"^export\s+\*\s+from\s+['"]([^'"]+)['"]"#).unwrap(),
                    Regex::new(r#"^export\s+\{[^}]*\}\s+from\s+['"]([^'"]+)['"]"#).unwrap(),
                ],
                require: Regex::new(r#"require\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap(),
                dynamic_import: Regex::new(r#"import\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap(),
                py_import: vec![
                    Regex::new(r"^from\s+(\.+\w*)\s+import\s+").unwrap(),
                ],
            },
            ready: false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn build_from_files(&mut self, project_root: &str, files: &[(String, String)]) {
        self.clear();
        self.project_root = project_root.to_string();

        self.load_tsconfig_paths(project_root);

        self.file_set = files.iter().map(|(p, _)| p.clone()).collect();

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
                self.parse_imports(path, &content);
            }
        }

        self.compute_centrality();
        self.ready = true;
    }

    pub fn get_dependencies(&self, file_path: &str) -> Vec<String> {
        self.forward_deps
            .get(file_path)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_dependents(&self, file_path: &str) -> Vec<String> {
        self.reverse_deps
            .get(file_path)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_centrality(&self, file_path: &str) -> f64 {
        self.centrality_cache.get(file_path).copied().unwrap_or(0.0)
    }

    pub fn expand(
        &self,
        file_path: &str,
        depth: usize,
        direction: ExpandDirection,
    ) -> Vec<String> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((file_path.to_string(), 0usize));

        while let Some((file, level)) = queue.pop_front() {
            if visited.contains(&file) || level > depth {
                continue;
            }
            visited.insert(file.clone());

            if matches!(direction, ExpandDirection::Forward | ExpandDirection::Both) {
                for dep in self.get_dependencies(&file) {
                    if !visited.contains(&dep) {
                        queue.push_back((dep, level + 1));
                    }
                }
            }
            if matches!(direction, ExpandDirection::Reverse | ExpandDirection::Both) {
                for dep in self.get_dependents(&file) {
                    if !visited.contains(&dep) {
                        queue.push_back((dep, level + 1));
                    }
                }
            }
        }

        visited.remove(file_path);
        visited.into_iter().collect()
    }

    pub fn get_top_central_files(&self, n: usize) -> Vec<FileCentrality> {
        let mut all_files = HashSet::new();
        all_files.extend(self.forward_deps.keys().cloned());
        all_files.extend(self.reverse_deps.keys().cloned());

        let mut results: Vec<FileCentrality> = all_files
            .into_iter()
            .map(|file_path| {
                let in_degree = self
                    .reverse_deps
                    .get(&file_path)
                    .map(|s| s.len())
                    .unwrap_or(0);
                let out_degree = self
                    .forward_deps
                    .get(&file_path)
                    .map(|s| s.len())
                    .unwrap_or(0);
                let centrality = self.centrality_cache.get(&file_path).copied().unwrap_or(0.0);
                FileCentrality {
                    file_path,
                    in_degree,
                    out_degree,
                    centrality,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.centrality
                .partial_cmp(&a.centrality)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(n);
        results
    }

    pub fn get_stats(&self) -> DependencyGraphStats {
        let mut all_files = HashSet::new();
        all_files.extend(self.forward_deps.keys());
        all_files.extend(self.reverse_deps.keys());
        DependencyGraphStats {
            total_files: all_files.len(),
            total_edges: self.edges.len(),
            is_ready: self.ready,
        }
    }

    pub fn clear(&mut self) {
        self.forward_deps.clear();
        self.reverse_deps.clear();
        self.edges.clear();
        self.centrality_cache.clear();
        self.file_set.clear();
        self.path_aliases.clear();
        self.project_root.clear();
        self.ready = false;
    }

    pub fn remove_file(&mut self, file_path: &str) {
        if let Some(targets) = self.forward_deps.remove(file_path) {
            for target in &targets {
                if let Some(rev) = self.reverse_deps.get_mut(target) {
                    rev.remove(file_path);
                    if rev.is_empty() {
                        self.reverse_deps.remove(target);
                    }
                }
            }
        }

        if let Some(sources) = self.reverse_deps.remove(file_path) {
            for source in &sources {
                if let Some(fwd) = self.forward_deps.get_mut(source) {
                    fwd.remove(file_path);
                    if fwd.is_empty() {
                        self.forward_deps.remove(source);
                    }
                }
            }
        }

        self.edges
            .retain(|e| e.from != file_path && e.to != file_path);
        self.centrality_cache.remove(file_path);
        self.file_set.remove(file_path);

        self.compute_centrality();
    }

    pub fn update_file(&mut self, project_root: &str, file_path: &str) {
        if let Some(old_targets) = self.forward_deps.remove(file_path) {
            for target in &old_targets {
                if let Some(rev) = self.reverse_deps.get_mut(target) {
                    rev.remove(file_path);
                    if rev.is_empty() {
                        self.reverse_deps.remove(target);
                    }
                }
            }
        }
        self.edges.retain(|e| e.from != file_path);

        self.file_set.insert(file_path.to_string());
        self.project_root = project_root.to_string();

        let full_path = format!("{}/{}", project_root, file_path);
        let Ok(meta) = std::fs::metadata(&full_path) else {
            return;
        };
        if meta.len() > 500 * 1024 {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            self.parse_imports(file_path, &content);
        }

        self.compute_centrality();
    }

    pub fn to_snapshot(&self) -> DependencyGraphSnapshot {
        DependencyGraphSnapshot {
            forward_deps: self
                .forward_deps
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect(),
            reverse_deps: self
                .reverse_deps
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect(),
            edges: self.edges.clone(),
            centrality_cache: self
                .centrality_cache
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        }
    }

    pub fn from_snapshot(
        &mut self,
        snapshot: DependencyGraphSnapshot,
        project_root: &str,
        file_set: HashSet<String>,
    ) {
        self.clear();
        self.project_root = project_root.to_string();
        self.file_set = file_set;

        for (k, v) in snapshot.forward_deps {
            self.forward_deps.insert(k, v.into_iter().collect());
        }
        for (k, v) in snapshot.reverse_deps {
            self.reverse_deps.insert(k, v.into_iter().collect());
        }
        self.edges = snapshot.edges;
        self.centrality_cache = snapshot.centrality_cache.into_iter().collect();
        self.ready = true;
    }

    fn parse_imports(&mut self, file_path: &str, content: &str) {
        let ext = file_extension(file_path);
        let is_python = ext == ".py";

        let mut collected_edges: Vec<(String, String, ImportType)> = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//")
                || trimmed.starts_with('*')
                || trimmed.starts_with("/*")
            {
                continue;
            }
            if is_python && trimmed.starts_with('#') {
                continue;
            }

            // ES static imports
            for pat in &self.patterns.es_import {
                if let Some(caps) = pat.captures(trimmed) {
                    let module_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    if !module_path.is_empty() && is_internal(&self.path_aliases, module_path) {
                        if let Some(resolved) = resolve_module(&self.path_aliases, &self.file_set, file_path, module_path) {
                            collected_edges.push((file_path.to_string(), resolved, ImportType::Static));
                        }
                    }
                }
            }

            // re-exports
            for pat in &self.patterns.reexport {
                if let Some(caps) = pat.captures(trimmed) {
                    let module_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    if !module_path.is_empty() && is_internal(&self.path_aliases, module_path) {
                        if let Some(resolved) = resolve_module(&self.path_aliases, &self.file_set, file_path, module_path) {
                            collected_edges.push((file_path.to_string(), resolved, ImportType::Static));
                        }
                    }
                }
            }

            // require()
            for caps in self.patterns.require.captures_iter(line) {
                let module_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if !module_path.is_empty() && is_internal(&self.path_aliases, module_path) {
                    if let Some(resolved) = resolve_module(&self.path_aliases, &self.file_set, file_path, module_path) {
                        collected_edges.push((file_path.to_string(), resolved, ImportType::Require));
                    }
                }
            }

            // dynamic import()
            for caps in self.patterns.dynamic_import.captures_iter(line) {
                let module_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if !module_path.is_empty() && is_internal(&self.path_aliases, module_path) {
                    if let Some(resolved) = resolve_module(&self.path_aliases, &self.file_set, file_path, module_path) {
                        collected_edges.push((file_path.to_string(), resolved, ImportType::Dynamic));
                    }
                }
            }

            // Python relative imports
            if is_python {
                for pat in &self.patterns.py_import {
                    if let Some(caps) = pat.captures(trimmed) {
                        let py_module_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                        if let Some(resolved) =
                            resolve_python(&self.file_set, file_path, py_module_path)
                        {
                            collected_edges.push((file_path.to_string(), resolved, ImportType::Static));
                        }
                    }
                }
            }
        }

        for (from, to, import_type) in collected_edges {
            self.add_edge(&from, &to, import_type);
        }
    }

    fn add_edge(&mut self, from: &str, to: &str, import_type: ImportType) {
        if from == to {
            return;
        }
        self.forward_deps
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
        self.reverse_deps
            .entry(to.to_string())
            .or_default()
            .insert(from.to_string());
        self.edges.push(DependencyEdge {
            from: from.to_string(),
            to: to.to_string(),
            import_type,
        });
    }

    pub fn is_internal_path(&self, module_path: &str) -> bool {
        is_internal(&self.path_aliases, module_path)
    }
}

fn is_internal(path_aliases: &HashMap<String, String>, module_path: &str) -> bool {
    if module_path.starts_with("./") || module_path.starts_with("../") {
        return true;
    }
    for alias in path_aliases.keys() {
        if module_path.starts_with(alias.as_str()) {
            return true;
        }
    }
    false
}

fn resolve_module(
    path_aliases: &HashMap<String, String>,
    file_set: &HashSet<String>,
    from_file: &str,
    module_path: &str,
) -> Option<String> {
    let resolved_base = if module_path.starts_with("./") || module_path.starts_with("../") {
        let from_dir = parent_dir(from_file);
        normalize_path(&format!("{}/{}", from_dir, module_path))
    } else {
        let mut matched = false;
        let mut resolved = module_path.to_string();
        for (alias, target) in path_aliases {
            if module_path.starts_with(alias.as_str()) {
                resolved = format!("{}{}", target, &module_path[alias.len()..]);
                matched = true;
                break;
            }
        }
        if !matched {
            return None;
        }
        normalize_path(&resolved)
    };

    let extensions = [
        "", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs",
        "/index.ts", "/index.js",
        ".py", ".go", ".java", ".rs",
        ".c", ".h", ".cpp", ".cc", ".hpp",
        ".cs", ".php", ".rb", ".swift", ".kt",
    ];
    for ext in &extensions {
        let candidate = format!("{}{}", resolved_base, ext);
        if file_set.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_python(
    file_set: &HashSet<String>,
    from_file: &str,
    py_module_path: &str,
) -> Option<String> {
    let from_dir = parent_dir(from_file);
    let mut dot_count = 0;
    for ch in py_module_path.chars() {
        if ch == '.' {
            dot_count += 1;
        } else {
            break;
        }
    }
    let module_name = &py_module_path[dot_count..];

    let mut target_dir = from_dir;
    for _ in 1..dot_count {
        target_dir = parent_dir(&target_dir);
    }

    if !module_name.is_empty() {
        let module_file = module_name.replace('.', "/");
        let as_file = normalize_path(&format!("{}/{}.py", target_dir, module_file));
        if file_set.contains(&as_file) {
            return Some(as_file);
        }
        let as_package =
            normalize_path(&format!("{}/{}/__init__.py", target_dir, module_file));
        if file_set.contains(&as_package) {
            return Some(as_package);
        }
    } else {
        let init_file = normalize_path(&format!("{}/__init__.py", target_dir));
        if file_set.contains(&init_file) {
            return Some(init_file);
        }
    }
    None
}

impl DependencyGraph {
    fn load_tsconfig_paths(&mut self, project_root: &str) {
        self.path_aliases.clear();
        let tsconfig_path = format!("{}/tsconfig.json", project_root);
        let Ok(raw) = std::fs::read_to_string(&tsconfig_path) else {
            return;
        };

        let cleaned: String = raw
            .lines()
            .map(|line| {
                if let Some(idx) = line.find("//") {
                    &line[..idx]
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let Ok(tsconfig) = serde_json::from_str::<serde_json::Value>(&cleaned) else {
            return;
        };

        let base_url = tsconfig
            .pointer("/compilerOptions/baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let Some(paths) = tsconfig
            .pointer("/compilerOptions/paths")
            .and_then(|v| v.as_object())
        else {
            return;
        };

        for (alias, targets) in paths {
            let Some(first_target) = targets
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let alias_prefix = alias.trim_end_matches('*');
            let target_prefix = first_target.trim_end_matches('*');
            let resolved = normalize_path(&format!("{}/{}", base_url, target_prefix));
            self.path_aliases
                .insert(alias_prefix.to_string(), resolved);
        }
    }

    fn compute_centrality(&mut self) {
        self.centrality_cache.clear();
        let mut all_files = HashSet::new();
        all_files.extend(self.forward_deps.keys().cloned());
        all_files.extend(self.reverse_deps.keys().cloned());
        if all_files.is_empty() {
            return;
        }

        let n = all_files.len();
        let damping = 0.85_f64;
        let iterations = 20;
        let initial_score = 1.0 / n as f64;

        let files: Vec<String> = all_files.into_iter().collect();
        let file_idx: HashMap<&str, usize> =
            files.iter().enumerate().map(|(i, f)| (f.as_str(), i)).collect();
        let mut scores = vec![initial_score; n];

        for _ in 0..iterations {
            let mut new_scores = vec![(1.0 - damping) / n as f64; n];

            for (i, file) in files.iter().enumerate() {
                let out_links = self.forward_deps.get(file);
                let out_degree = out_links.map(|s| s.len()).unwrap_or(0);

                if out_degree == 0 {
                    let share = scores[i] * damping / n as f64;
                    for ns in new_scores.iter_mut() {
                        *ns += share;
                    }
                } else {
                    let share = scores[i] * damping / out_degree as f64;
                    for target in out_links.unwrap() {
                        if let Some(&ti) = file_idx.get(target.as_str()) {
                            new_scores[ti] += share;
                        }
                    }
                }
            }

            scores = new_scores;
        }

        let max_score = scores.iter().cloned().fold(0.0_f64, f64::max);
        for (i, file) in files.into_iter().enumerate() {
            let normalized = if max_score > 0.0 {
                scores[i] / max_score
            } else {
                0.0
            };
            self.centrality_cache.insert(file, normalized);
        }
    }
}

#[derive(Clone, Debug)]
pub struct DependencyGraphStats {
    pub total_files: usize,
    pub total_edges: usize,
    pub is_ready: bool,
}

fn file_extension(path: &str) -> &str {
    if let Some(pos) = path.rfind('.') {
        &path[pos..]
    } else {
        ""
    }
}

fn parent_dir(path: &str) -> String {
    if let Some(pos) = path.rfind('/') {
        path[..pos].to_string()
    } else {
        ".".to_string()
    }
}

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("src/utils/../lib/index"), "src/lib/index");
        assert_eq!(normalize_path("./src/lib"), "src/lib");
        assert_eq!(normalize_path("src/./lib"), "src/lib");
    }

    #[test]
    fn test_add_edge_and_query() {
        let mut g = DependencyGraph::new();
        g.file_set.insert("a.ts".to_string());
        g.file_set.insert("b.ts".to_string());
        g.file_set.insert("c.ts".to_string());

        g.add_edge("a.ts", "b.ts", ImportType::Static);
        g.add_edge("a.ts", "c.ts", ImportType::Static);
        g.add_edge("b.ts", "c.ts", ImportType::Dynamic);

        let deps = g.get_dependencies("a.ts");
        assert!(deps.contains(&"b.ts".to_string()));
        assert!(deps.contains(&"c.ts".to_string()));

        let dependents = g.get_dependents("c.ts");
        assert!(dependents.contains(&"a.ts".to_string()));
        assert!(dependents.contains(&"b.ts".to_string()));
    }

    #[test]
    fn test_expand_bfs() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "b.ts", ImportType::Static);
        g.add_edge("b.ts", "c.ts", ImportType::Static);
        g.add_edge("c.ts", "d.ts", ImportType::Static);

        let expanded = g.expand("a.ts", 1, ExpandDirection::Forward);
        assert!(expanded.contains(&"b.ts".to_string()));
        assert!(!expanded.contains(&"c.ts".to_string()));

        let expanded2 = g.expand("a.ts", 2, ExpandDirection::Forward);
        assert!(expanded2.contains(&"b.ts".to_string()));
        assert!(expanded2.contains(&"c.ts".to_string()));
        assert!(!expanded2.contains(&"d.ts".to_string()));

        let reverse = g.expand("c.ts", 1, ExpandDirection::Reverse);
        assert!(reverse.contains(&"b.ts".to_string()));
        assert!(!reverse.contains(&"a.ts".to_string()));
    }

    #[test]
    fn test_pagerank_centrality() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "hub.ts", ImportType::Static);
        g.add_edge("b.ts", "hub.ts", ImportType::Static);
        g.add_edge("c.ts", "hub.ts", ImportType::Static);
        g.add_edge("hub.ts", "d.ts", ImportType::Static);

        g.compute_centrality();

        let hub_centrality = g.get_centrality("hub.ts");
        let a_centrality = g.get_centrality("a.ts");
        assert!(
            hub_centrality > a_centrality,
            "hub should have higher centrality: hub={} a={}",
            hub_centrality,
            a_centrality
        );
    }

    #[test]
    fn test_remove_file() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "b.ts", ImportType::Static);
        g.add_edge("b.ts", "c.ts", ImportType::Static);
        g.file_set.insert("a.ts".to_string());
        g.file_set.insert("b.ts".to_string());
        g.file_set.insert("c.ts".to_string());

        g.remove_file("b.ts");

        assert!(g.get_dependencies("a.ts").is_empty());
        assert!(g.get_dependents("c.ts").is_empty());
        assert!(!g.file_set.contains("b.ts"));
    }

    #[test]
    fn test_parse_imports() {
        let mut g = DependencyGraph::new();
        g.file_set.insert("src/index.ts".to_string());
        g.file_set.insert("src/utils.ts".to_string());
        g.file_set.insert("src/lib/helper.ts".to_string());

        let content = r#"import { foo } from './utils';
import helper from './lib/helper';
const x = require('./utils');
const y = import('./lib/helper');
"#;
        g.parse_imports("src/index.ts", content);

        let deps = g.get_dependencies("src/index.ts");
        assert!(deps.contains(&"src/utils.ts".to_string()));
        assert!(deps.contains(&"src/lib/helper.ts".to_string()));
    }

    #[test]
    fn test_python_imports() {
        let mut g = DependencyGraph::new();
        g.file_set.insert("app/services/user.py".to_string());
        g.file_set.insert("app/services/auth.py".to_string());
        g.file_set.insert("app/__init__.py".to_string());

        let content = r#"from .auth import verify_token
from ..app import create_app
"#;
        g.parse_imports("app/services/user.py", content);

        let deps = g.get_dependencies("app/services/user.py");
        assert!(deps.contains(&"app/services/auth.py".to_string()));
    }

    #[test]
    fn test_self_import_ignored() {
        let mut g = DependencyGraph::new();
        g.file_set.insert("src/index.ts".to_string());

        g.add_edge("src/index.ts", "src/index.ts", ImportType::Static);
        assert!(g.get_dependencies("src/index.ts").is_empty());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "b.ts", ImportType::Static);
        g.compute_centrality();
        g.ready = true;

        let snapshot = g.to_snapshot();
        let mut g2 = DependencyGraph::new();
        let file_set: HashSet<String> =
            ["a.ts", "b.ts"].iter().map(|s| s.to_string()).collect();
        g2.from_snapshot(snapshot, "", file_set);

        assert!(g2.is_ready());
        assert!(g2.get_dependencies("a.ts").contains(&"b.ts".to_string()));
    }

    #[test]
    fn test_is_internal_path() {
        let mut g = DependencyGraph::new();
        assert!(g.is_internal_path("./utils"));
        assert!(g.is_internal_path("../lib/helper"));
        assert!(!g.is_internal_path("react"));
        assert!(!g.is_internal_path("@types/node"));

        g.path_aliases.insert("@/".to_string(), "src/".to_string());
        assert!(g.is_internal_path("@/utils"));
    }

    #[test]
    fn test_top_central_files() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "hub.ts", ImportType::Static);
        g.add_edge("b.ts", "hub.ts", ImportType::Static);
        g.add_edge("c.ts", "hub.ts", ImportType::Static);
        g.compute_centrality();

        let top = g.get_top_central_files(2);
        assert!(!top.is_empty());
        assert_eq!(top[0].file_path, "hub.ts");
    }
}
