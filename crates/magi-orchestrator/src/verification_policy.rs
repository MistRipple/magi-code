use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    Standard,
    Smoke,
}

pub struct VerificationModeInput<'a> {
    pub task_title: Option<&'a str>,
    pub goal: Option<&'a str>,
    pub analysis: Option<&'a str>,
    pub acceptance: Option<&'a [String]>,
    pub constraints: Option<&'a [String]>,
    pub context: Option<&'a [String]>,
}

const SMOKE_PATTERNS: &[&str] = &[
    "快速验证", "快速测试", "快速 smoke", "smoke test", "smoke", "烟测",
    "连通性验证", "链路验证", "编排测试", "文件编辑测试",
];

const LOW_COST_PATTERNS: &[&str] = &[
    "不用很麻烦", "别太复杂", "简单验证", "简单测试", "快速完成", "只做验证", "仅做验证",
];

const QUICK_PATTERNS: &[&str] = &["快速", "简单", "只做", "仅做", "轻量"];

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    let lower = text.to_lowercase();
    patterns.iter().any(|p| lower.contains(&p.to_lowercase()))
}

pub fn resolve_verification_mode(input: &VerificationModeInput) -> VerificationMode {
    let mut parts = Vec::new();

    if let Some(v) = input.task_title {
        let trimmed = v.trim();
        if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
    }
    if let Some(v) = input.goal {
        let trimmed = v.trim();
        if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
    }
    if let Some(v) = input.analysis {
        let trimmed = v.trim();
        if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
    }
    if let Some(items) = input.acceptance {
        for item in items.iter() {
            let trimmed = item.trim();
            if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
        }
    }
    if let Some(items) = input.constraints {
        for item in items.iter() {
            let trimmed = item.trim();
            if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
        }
    }
    if let Some(items) = input.context {
        for item in items.iter() {
            let trimmed = item.trim();
            if !trimmed.is_empty() { parts.push(trimmed.to_string()); }
        }
    }

    let combined = parts.join("\n");
    if combined.is_empty() {
        return VerificationMode::Standard;
    }

    if contains_any(&combined, SMOKE_PATTERNS) && contains_any(&combined, LOW_COST_PATTERNS) {
        return VerificationMode::Smoke;
    }

    if contains_any(&combined, SMOKE_PATTERNS) && contains_any(&combined, QUICK_PATTERNS) {
        return VerificationMode::Smoke;
    }

    VerificationMode::Standard
}

pub fn is_smoke_verification_input(input: &VerificationModeInput) -> bool {
    resolve_verification_mode(input) == VerificationMode::Smoke
}

use super::verification_runner::VerificationConfig;

pub fn resolve_verification_config_overrides(mode: VerificationMode) -> Option<VerificationConfig> {
    match mode {
        VerificationMode::Smoke => Some(VerificationConfig {
            compile_check: false,
            lint_check: false,
            test_check: false,
            ide_check: true,
            ..VerificationConfig::default()
        }),
        VerificationMode::Standard => None,
    }
}
