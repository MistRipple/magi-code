use std::collections::HashSet;

use super::batch::{DispatchBatch, DispatchCollaborationContracts};

pub struct DispatchCorrelationPlanInput<'a> {
    pub batch: &'a DispatchBatch,
    pub depends_on: Option<Vec<String>>,
    pub files: Option<Vec<String>>,
    pub scope_hint: Option<Vec<String>>,
    pub collaboration_contracts: DispatchCollaborationContracts,
}

#[derive(Clone, Debug)]
pub struct DispatchCorrelationPlanResult {
    pub depends_on: Option<Vec<String>>,
    pub added_dependencies: Vec<String>,
    pub reasons: Vec<String>,
}

struct CorrelationIntent {
    target_paths: HashSet<String>,
    producer_contracts: HashSet<String>,
    consumer_contracts: HashSet<String>,
    interface_symbols: HashSet<String>,
    freeze_paths: HashSet<String>,
}

pub struct DispatchCorrelationPlanner;

impl DispatchCorrelationPlanner {
    pub fn new() -> Self {
        Self
    }

    pub fn plan(&self, input: &DispatchCorrelationPlanInput<'_>) -> DispatchCorrelationPlanResult {
        let base_depends_on: Vec<String> = input
            .depends_on
            .as_ref()
            .map(|deps| {
                let mut seen = HashSet::new();
                deps.iter()
                    .filter(|d| seen.insert(d.as_str().to_string()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut dependency_set: HashSet<String> = base_depends_on.iter().cloned().collect();
        let mut reasons = HashSet::new();
        let mut result_deps = base_depends_on.clone();

        let source_intent = self.build_intent(
            input.files.as_deref(),
            input.scope_hint.as_deref(),
            &input.collaboration_contracts,
        );

        for entry in input.batch.entries() {
            if entry.status.is_terminal() {
                continue;
            }
            if dependency_set.contains(&entry.task_id) {
                continue;
            }

            let target_intent = self.build_intent(
                Some(&entry.task_contract.files),
                Some(&entry.task_contract.scope_hint),
                &entry.task_contract.collaboration_contracts,
            );

            if let Some(relation) = self.detect_relation(&source_intent, &target_intent) {
                dependency_set.insert(entry.task_id.clone());
                result_deps.push(entry.task_id.clone());
                reasons.insert(relation);
            }
        }

        let original_set: HashSet<&str> = input
            .depends_on
            .as_ref()
            .map(|deps| deps.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        let added_dependencies: Vec<String> = result_deps
            .iter()
            .filter(|id| !original_set.contains(id.as_str()))
            .cloned()
            .collect();

        DispatchCorrelationPlanResult {
            depends_on: if result_deps.is_empty() {
                None
            } else {
                Some(result_deps)
            },
            added_dependencies,
            reasons: reasons.into_iter().collect(),
        }
    }

    fn build_intent(
        &self,
        files: Option<&[String]>,
        scope_hint: Option<&[String]>,
        contracts: &DispatchCollaborationContracts,
    ) -> CorrelationIntent {
        let mut target_paths = HashSet::new();
        if let Some(files) = files {
            for f in files {
                let normalized = normalize_path(f);
                if !normalized.is_empty() {
                    target_paths.insert(normalized);
                }
            }
        }
        if let Some(hints) = scope_hint {
            for h in hints {
                let normalized = normalize_path(h);
                if !normalized.is_empty() {
                    target_paths.insert(normalized);
                }
            }
        }

        let producer_contracts: HashSet<String> = contracts
            .producer_contracts
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let consumer_contracts: HashSet<String> = contracts
            .consumer_contracts
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let interface_symbols = extract_interface_symbols(&contracts.interface_contracts);

        let freeze_paths: HashSet<String> = contracts
            .freeze_files
            .iter()
            .map(|s| normalize_path(s))
            .filter(|s| !s.is_empty())
            .collect();

        CorrelationIntent {
            target_paths,
            producer_contracts,
            consumer_contracts,
            interface_symbols,
            freeze_paths,
        }
    }

    fn detect_relation(
        &self,
        source: &CorrelationIntent,
        target: &CorrelationIntent,
    ) -> Option<String> {
        if intersects(&source.target_paths, &target.target_paths) {
            return Some("same_file".to_string());
        }
        if intersects(&source.freeze_paths, &target.target_paths)
            || intersects(&target.freeze_paths, &source.target_paths)
        {
            return Some("freeze_file".to_string());
        }
        if intersects(&source.consumer_contracts, &target.producer_contracts)
            || intersects(&target.consumer_contracts, &source.producer_contracts)
        {
            return Some("contract_dependency".to_string());
        }
        if intersects(&source.interface_symbols, &target.interface_symbols) {
            return Some("interface_symbol".to_string());
        }
        None
    }
}

impl Default for DispatchCorrelationPlanner {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_path(input: &str) -> String {
    input.replace('\\', "/").trim_start_matches("./").trim().to_string()
}

fn intersects(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    left.iter().any(|v| right.contains(v))
}

fn extract_interface_symbols(contracts: &[String]) -> HashSet<String> {
    use regex::Regex;
    let mut symbols = HashSet::new();

    let pascal_re = Regex::new(r"\b[A-Z][a-zA-Z0-9]{2,}\b").unwrap();
    let generic_re = Regex::new(r"<([A-Z][a-zA-Z0-9]*)>").unwrap();
    let extends_re = Regex::new(r"(?:extends|implements)\s+([A-Z][a-zA-Z0-9]*)").unwrap();

    for contract in contracts {
        for cap in pascal_re.find_iter(contract) {
            symbols.insert(cap.as_str().to_string());
        }
        for cap in generic_re.captures_iter(contract) {
            if let Some(inner) = cap.get(1) {
                let s = inner.as_str();
                if s.len() >= 2 {
                    symbols.insert(s.to_string());
                }
            }
        }
        for cap in extends_re.captures_iter(contract) {
            if let Some(type_name) = cap.get(1) {
                let s = type_name.as_str();
                if s.len() >= 2 {
                    symbols.insert(s.to_string());
                }
            }
        }
    }

    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::batch::DispatchTaskContract;

    fn make_batch_with_entries() -> DispatchBatch {
        let mut batch = DispatchBatch::new(Some("test-batch"));
        batch
            .register(
                "task-a",
                "worker-1",
                DispatchTaskContract {
                    task_title: "修改 auth 模块".to_string(),
                    files: vec!["src/auth/login.rs".to_string()],
                    scope_hint: vec![],
                    collaboration_contracts: DispatchCollaborationContracts {
                        producer_contracts: vec!["AuthToken".to_string()],
                        consumer_contracts: vec![],
                        interface_contracts: vec!["UserService".to_string()],
                        freeze_files: vec![],
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        batch
            .register(
                "task-b",
                "worker-2",
                DispatchTaskContract {
                    task_title: "修改 UI 页面".to_string(),
                    files: vec!["src/ui/page.tsx".to_string()],
                    scope_hint: vec![],
                    collaboration_contracts: DispatchCollaborationContracts::default(),
                    ..Default::default()
                },
            )
            .unwrap();
        batch
    }

    #[test]
    fn no_correlation_for_independent_tasks() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: None,
            files: Some(vec!["src/config.rs".to_string()]),
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts::default(),
        });
        assert!(result.added_dependencies.is_empty());
    }

    #[test]
    fn same_file_creates_dependency() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: None,
            files: Some(vec!["src/auth/login.rs".to_string()]),
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts::default(),
        });
        assert!(result.added_dependencies.contains(&"task-a".to_string()));
        assert!(result.reasons.contains(&"same_file".to_string()));
    }

    #[test]
    fn contract_dependency_detected() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: None,
            files: None,
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts {
                consumer_contracts: vec!["AuthToken".to_string()],
                producer_contracts: vec![],
                interface_contracts: vec![],
                freeze_files: vec![],
            },
        });
        assert!(result.added_dependencies.contains(&"task-a".to_string()));
        assert!(result.reasons.contains(&"contract_dependency".to_string()));
    }

    #[test]
    fn interface_symbol_correlation() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: None,
            files: None,
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts {
                interface_contracts: vec!["implements UserService".to_string()],
                producer_contracts: vec![],
                consumer_contracts: vec![],
                freeze_files: vec![],
            },
        });
        assert!(result.added_dependencies.contains(&"task-a".to_string()));
        assert!(result.reasons.contains(&"interface_symbol".to_string()));
    }

    #[test]
    fn freeze_file_creates_dependency() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: None,
            files: None,
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts {
                freeze_files: vec!["src/auth/login.rs".to_string()],
                producer_contracts: vec![],
                consumer_contracts: vec![],
                interface_contracts: vec![],
            },
        });
        assert!(result.added_dependencies.contains(&"task-a".to_string()));
        assert!(result.reasons.contains(&"freeze_file".to_string()));
    }

    #[test]
    fn existing_dependencies_preserved() {
        let batch = make_batch_with_entries();
        let planner = DispatchCorrelationPlanner::new();
        let result = planner.plan(&DispatchCorrelationPlanInput {
            batch: &batch,
            depends_on: Some(vec!["existing-dep".to_string()]),
            files: None,
            scope_hint: None,
            collaboration_contracts: DispatchCollaborationContracts::default(),
        });
        assert!(result.depends_on.as_ref().unwrap().contains(&"existing-dep".to_string()));
        assert!(result.added_dependencies.is_empty());
    }
}
