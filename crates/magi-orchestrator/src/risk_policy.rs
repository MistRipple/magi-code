use magi_core::RiskLevel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskPath {
    Light,
    Standard,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    None,
    Basic,
    Full,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub path: RiskPath,
    pub hard_stop: bool,
    pub verification: VerificationLevel,
    pub score: u32,
    pub signals: Vec<String>,
}

pub struct RiskPolicyInput<'a> {
    pub prompt: &'a str,
    pub analysis: Option<&'a str>,
    pub feature_contract: Option<&'a str>,
    pub sub_task_count: usize,
    pub target_files: &'a [String],
    pub acceptance_criteria_count: usize,
    pub failure_rate: Option<f64>,
}

pub fn evaluate_risk(input: &RiskPolicyInput) -> RiskAssessment {
    let mut signals = Vec::new();
    let mut score: u32 = 0;

    let target_files: HashSet<&str> = input.target_files.iter().map(|s| s.as_str()).collect();
    let file_count = if target_files.is_empty() {
        input.sub_task_count
    } else {
        target_files.len()
    };

    let file_score = if file_count == 0 {
        0
    } else if file_count <= 2 {
        1
    } else if file_count <= 5 {
        2
    } else {
        3
    };
    if file_score > 0 {
        score += file_score * 2;
        signals.push(format!("file_count_{file_score}"));
    }

    let module_count = count_modules(&target_files);
    let module_score = if module_count == 0 {
        0
    } else if module_count == 1 {
        1
    } else {
        2
    };
    if module_score > 0 {
        score += module_score * 3;
        signals.push(format!("module_count_{module_score}"));
    }

    if has_interface_change(input.prompt, input.analysis, input.feature_contract) {
        score += 3 * 4;
        signals.push("interface_change".to_string());
    }

    if has_config_change(&target_files) {
        score += 3 * 4;
        signals.push("config_or_dependency_change".to_string());
    }

    if let Some(failure_rate) = input.failure_rate
        && failure_rate >= 0.0
    {
        let failure_score = if failure_rate > 0.3 {
            2
        } else if failure_rate >= 0.1 {
            1
        } else {
            0
        };
        if failure_score > 0 {
            score += failure_score * 2;
            signals.push(format!("failure_rate_{failure_score}"));
        }
    }

    if target_files.is_empty() && input.sub_task_count > 1 {
        score += 6;
        signals.push("unknown_file_scope".to_string());
    }

    let level = if score >= 13 {
        RiskLevel::High
    } else if score >= 7 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    let path = match level {
        RiskLevel::Low => RiskPath::Light,
        RiskLevel::Medium => RiskPath::Standard,
        RiskLevel::High => RiskPath::Full,
    };

    let hard_stop = level != RiskLevel::Low;
    let verification = if level == RiskLevel::High {
        VerificationLevel::Full
    } else {
        VerificationLevel::Basic
    };

    RiskAssessment {
        level,
        path,
        hard_stop,
        verification,
        score,
        signals,
    }
}

fn count_modules(files: &HashSet<&str>) -> usize {
    if files.is_empty() {
        return 0;
    }
    let mut modules = HashSet::new();
    for file in files {
        let normalized = file.replace('\\', "/");
        let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
        if let Some(segment) = normalized.split('/').next()
            && !segment.is_empty()
        {
            modules.insert(segment.to_string());
        }
    }
    modules.len()
}

fn has_interface_change(
    prompt: &str,
    analysis: Option<&str>,
    feature_contract: Option<&str>,
) -> bool {
    let combined = format!(
        "{}\n{}\n{}",
        prompt,
        analysis.unwrap_or(""),
        feature_contract.unwrap_or("")
    );
    if combined.trim().is_empty() {
        return false;
    }
    const KEYWORDS: &[&str] = &[
        "API", "接口", "endpoint", "schema", "契约", "请求", "响应", "字段", "payload",
    ];
    KEYWORDS.iter().any(|kw| combined.contains(kw))
}

fn has_config_change(files: &HashSet<&str>) -> bool {
    if files.is_empty() {
        return false;
    }
    const CONFIG_FILES: &[&str] = &[
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "requirements.txt",
        "pyproject.toml",
        "Pipfile",
        "go.mod",
        "go.sum",
        "Cargo.toml",
        "Cargo.lock",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "tsconfig.json",
        "vite.config",
        "webpack.config",
        "next.config",
    ];
    for file in files {
        let base = file.rsplit('/').next().unwrap_or(file);
        if CONFIG_FILES
            .iter()
            .any(|cfg| base == *cfg || base.starts_with(cfg))
        {
            return true;
        }
    }
    false
}
