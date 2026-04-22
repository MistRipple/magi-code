use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::entry_router::{EntryPath, PlanMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceDecision {
    Ask,
    Auto,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceThresholds {
    pub c_min: f64,
    pub c_ok: f64,
    pub r_low: f64,
    pub r_high: f64,
}

impl Default for GovernanceThresholds {
    fn default() -> Self {
        Self {
            c_min: 0.55,
            c_ok: 0.75,
            r_low: 0.35,
            r_high: 0.70,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanGovernanceAssessment {
    pub risk_score: f64,
    pub confidence: f64,
    pub affected_files: usize,
    pub cross_modules: usize,
    pub write_tool_ratio: f64,
    pub historical_failure_rate: f64,
    pub source_coverage: f64,
    pub signal_agreement: f64,
    pub historical_calibration: f64,
    pub decision: GovernanceDecision,
    pub reasons: Vec<String>,
}

pub struct GovernanceRiskInput {
    pub affected_files: usize,
    pub cross_modules: usize,
    pub write_tool_ratio: f64,
    pub historical_failure_rate: f64,
}

pub struct GovernanceConfidenceInput {
    pub source_coverage: f64,
    pub signal_agreement: f64,
    pub historical_calibration: f64,
}

pub fn compute_jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

pub fn normalize_threshold_value(raw: Option<f64>, fallback: f64) -> f64 {
    match raw {
        Some(v) if v.is_finite() && (0.0..=1.0).contains(&v) => v,
        _ => fallback,
    }
}

pub fn validate_governance_thresholds(
    c_min: Option<f64>,
    c_ok: Option<f64>,
    r_low: Option<f64>,
    r_high: Option<f64>,
) -> GovernanceThresholds {
    let defaults = GovernanceThresholds::default();
    let mut t = GovernanceThresholds {
        c_min: normalize_threshold_value(c_min, defaults.c_min),
        c_ok: normalize_threshold_value(c_ok, defaults.c_ok),
        r_low: normalize_threshold_value(r_low, defaults.r_low),
        r_high: normalize_threshold_value(r_high, defaults.r_high),
    };

    if t.c_min > t.c_ok {
        t.c_min = defaults.c_min;
        t.c_ok = defaults.c_ok;
    }
    if t.r_low > t.r_high {
        t.r_low = defaults.r_low;
        t.r_high = defaults.r_high;
    }

    t
}

pub fn estimate_write_tool_ratio(
    entry_path: EntryPath,
    requires_modification: bool,
    prompt: &str,
    planning_mode: PlanMode,
) -> f64 {
    if entry_path != EntryPath::TaskExecution {
        return 0.15;
    }
    if requires_modification {
        return if planning_mode == PlanMode::Deep {
            0.9
        } else {
            0.7
        };
    }

    let lower = prompt.to_lowercase();
    let read_only_keywords = [
        "总结", "分析", "读取", "解释", "review", "summarize", "read only", "diagnose",
    ];
    if read_only_keywords.iter().any(|kw| lower.contains(kw)) {
        return 0.2;
    }

    if planning_mode == PlanMode::Deep {
        0.55
    } else {
        0.45
    }
}

pub fn compute_governance_risk_score(input: &GovernanceRiskInput) -> f64 {
    let normalize_files = (input.affected_files.max(1) as f64 / 40.0).min(1.0);
    let normalize_modules = (input.cross_modules.max(1) as f64 / 8.0).min(1.0);
    (0.35 * normalize_files
        + 0.25 * normalize_modules
        + 0.20 * input.write_tool_ratio
        + 0.20 * input.historical_failure_rate)
        .min(1.0)
}

pub fn compute_governance_confidence(input: &GovernanceConfidenceInput) -> f64 {
    (0.4 * input.source_coverage + 0.4 * input.signal_agreement + 0.2 * input.historical_calibration)
        .min(1.0)
}

pub fn compute_historical_calibration(sample_count: usize) -> f64 {
    if sample_count >= 12 {
        0.9
    } else if sample_count >= 8 {
        0.75
    } else if sample_count >= 4 {
        0.55
    } else {
        0.35
    }
}

pub fn evaluate_governance_decision(
    risk_score: f64,
    confidence: f64,
    source_coverage: f64,
    thresholds: &GovernanceThresholds,
) -> (GovernanceDecision, Vec<String>) {
    let mut reasons = Vec::new();

    if source_coverage < 2.0 / 3.0 {
        reasons.push(format!("coverage<2/3({:.2})", source_coverage));
    }
    if confidence < thresholds.c_min {
        reasons.push(format!(
            "confidence<C_min({:.2}<{})",
            confidence, thresholds.c_min
        ));
    }
    if risk_score >= thresholds.r_high {
        reasons.push(format!(
            "risk>=R_high({:.2}>={:.2})",
            risk_score, thresholds.r_high
        ));
    }

    if reasons.is_empty() && risk_score <= thresholds.r_low && confidence >= thresholds.c_ok {
        return (GovernanceDecision::Auto, reasons);
    }

    if reasons.is_empty() {
        reasons.push(format!(
            "gray_zone(risk={:.2},confidence={:.2})",
            risk_score, confidence
        ));
    }

    (GovernanceDecision::Ask, reasons)
}

pub fn build_fallback_governance_assessment(error_message: &str) -> PlanGovernanceAssessment {
    let clipped: String = error_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();

    PlanGovernanceAssessment {
        risk_score: 1.0,
        confidence: 0.0,
        affected_files: 1,
        cross_modules: 1,
        write_tool_ratio: 1.0,
        historical_failure_rate: 0.5,
        source_coverage: 0.0,
        signal_agreement: 0.0,
        historical_calibration: 0.0,
        decision: GovernanceDecision::Ask,
        reasons: vec![format!(
            "assessment_error({})",
            if clipped.is_empty() {
                "unknown"
            } else {
                &clipped
            }
        )],
    }
}

pub fn extract_path_like_candidates(prompt: &str) -> Vec<String> {
    if prompt.trim().is_empty() {
        return Vec::new();
    }
    let re = regex::Regex::new(r"[A-Za-z0-9._/-]+\.[A-Za-z0-9]+").unwrap();
    let mut seen = HashSet::new();
    re.find_iter(prompt)
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

pub fn extract_prompt_tokens(prompt: &str) -> Vec<String> {
    let lower = prompt.to_lowercase();
    let re = regex::Regex::new(r"[a-z0-9_]{3,}").unwrap();
    let mut seen = HashSet::new();
    re.find_iter(&lower)
        .map(|m| m.as_str().to_string())
        .filter(|s| seen.insert(s.clone()))
        .take(24)
        .collect()
}

pub fn normalize_relative_path(input: &str) -> String {
    input
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim()
        .to_string()
}

pub fn infer_module_from_path(file: &str) -> String {
    let normalized = normalize_relative_path(file);
    if normalized.is_empty() {
        return String::new();
    }
    normalized.split('/').next().unwrap_or(&normalized).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_both_empty() {
        let a: HashSet<String> = HashSet::new();
        let b: HashSet<String> = HashSet::new();
        assert!((compute_jaccard(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_one_empty() {
        let a: HashSet<String> = ["x".to_string()].into_iter().collect();
        let b: HashSet<String> = HashSet::new();
        assert!((compute_jaccard(&a, &b)).abs() < 1e-9);
    }

    #[test]
    fn jaccard_identical() {
        let a: HashSet<String> = ["x".to_string(), "y".to_string()].into_iter().collect();
        assert!((compute_jaccard(&a, &a) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let b: HashSet<String> = ["b".to_string(), "c".to_string()].into_iter().collect();
        assert!((compute_jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint() {
        let a: HashSet<String> = ["a".to_string()].into_iter().collect();
        let b: HashSet<String> = ["b".to_string()].into_iter().collect();
        assert!((compute_jaccard(&a, &b)).abs() < 1e-9);
    }

    #[test]
    fn threshold_valid_value() {
        assert!((normalize_threshold_value(Some(0.5), 0.8) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn threshold_out_of_range() {
        assert!((normalize_threshold_value(Some(1.5), 0.8) - 0.8).abs() < 1e-9);
        assert!((normalize_threshold_value(Some(-0.1), 0.8) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn threshold_nan_fallback() {
        assert!((normalize_threshold_value(Some(f64::NAN), 0.8) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn threshold_none_fallback() {
        assert!((normalize_threshold_value(None, 0.8) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn validate_thresholds_normal() {
        let t = validate_governance_thresholds(Some(0.5), Some(0.7), Some(0.3), Some(0.6));
        assert!((t.c_min - 0.5).abs() < 1e-9);
        assert!((t.c_ok - 0.7).abs() < 1e-9);
        assert!((t.r_low - 0.3).abs() < 1e-9);
        assert!((t.r_high - 0.6).abs() < 1e-9);
    }

    #[test]
    fn validate_thresholds_c_min_gt_c_ok_reset() {
        let t = validate_governance_thresholds(Some(0.9), Some(0.3), Some(0.3), Some(0.6));
        let defaults = GovernanceThresholds::default();
        assert!((t.c_min - defaults.c_min).abs() < 1e-9);
        assert!((t.c_ok - defaults.c_ok).abs() < 1e-9);
    }

    #[test]
    fn validate_thresholds_r_low_gt_r_high_reset() {
        let t = validate_governance_thresholds(Some(0.5), Some(0.7), Some(0.8), Some(0.3));
        let defaults = GovernanceThresholds::default();
        assert!((t.r_low - defaults.r_low).abs() < 1e-9);
        assert!((t.r_high - defaults.r_high).abs() < 1e-9);
    }

    #[test]
    fn write_tool_ratio_non_task() {
        let r = estimate_write_tool_ratio(EntryPath::DirectResponse, false, "", PlanMode::Standard);
        assert!((r - 0.15).abs() < 1e-9);
    }

    #[test]
    fn write_tool_ratio_modification_deep() {
        let r = estimate_write_tool_ratio(EntryPath::TaskExecution, true, "", PlanMode::Deep);
        assert!((r - 0.9).abs() < 1e-9);
    }

    #[test]
    fn write_tool_ratio_modification_standard() {
        let r = estimate_write_tool_ratio(EntryPath::TaskExecution, true, "", PlanMode::Standard);
        assert!((r - 0.7).abs() < 1e-9);
    }

    #[test]
    fn write_tool_ratio_read_only_keyword() {
        let r = estimate_write_tool_ratio(
            EntryPath::TaskExecution,
            false,
            "请分析一下这个模块",
            PlanMode::Standard,
        );
        assert!((r - 0.2).abs() < 1e-9);
    }

    #[test]
    fn write_tool_ratio_ambiguous_deep() {
        let r = estimate_write_tool_ratio(
            EntryPath::TaskExecution,
            false,
            "处理这个任务",
            PlanMode::Deep,
        );
        assert!((r - 0.55).abs() < 1e-9);
    }

    #[test]
    fn risk_score_low_input() {
        let score = compute_governance_risk_score(&GovernanceRiskInput {
            affected_files: 2,
            cross_modules: 1,
            write_tool_ratio: 0.1,
            historical_failure_rate: 0.0,
        });
        assert!(score < 0.15);
    }

    #[test]
    fn risk_score_high_input() {
        let score = compute_governance_risk_score(&GovernanceRiskInput {
            affected_files: 50,
            cross_modules: 10,
            write_tool_ratio: 0.9,
            historical_failure_rate: 0.8,
        });
        assert!(score > 0.8);
    }

    #[test]
    fn risk_score_capped_at_one() {
        let score = compute_governance_risk_score(&GovernanceRiskInput {
            affected_files: 100,
            cross_modules: 20,
            write_tool_ratio: 1.0,
            historical_failure_rate: 1.0,
        });
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn confidence_full_coverage() {
        let c = compute_governance_confidence(&GovernanceConfidenceInput {
            source_coverage: 1.0,
            signal_agreement: 1.0,
            historical_calibration: 1.0,
        });
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn confidence_no_data() {
        let c = compute_governance_confidence(&GovernanceConfidenceInput {
            source_coverage: 0.0,
            signal_agreement: 0.0,
            historical_calibration: 0.0,
        });
        assert!(c.abs() < 1e-9);
    }

    #[test]
    fn historical_calibration_values() {
        assert!((compute_historical_calibration(15) - 0.9).abs() < 1e-9);
        assert!((compute_historical_calibration(10) - 0.75).abs() < 1e-9);
        assert!((compute_historical_calibration(5) - 0.55).abs() < 1e-9);
        assert!((compute_historical_calibration(2) - 0.35).abs() < 1e-9);
    }

    #[test]
    fn decision_auto_low_risk_high_confidence() {
        let thresholds = GovernanceThresholds::default();
        let (decision, reasons) = evaluate_governance_decision(0.2, 0.8, 1.0, &thresholds);
        assert_eq!(decision, GovernanceDecision::Auto);
        assert!(reasons.is_empty());
    }

    #[test]
    fn decision_ask_high_risk() {
        let thresholds = GovernanceThresholds::default();
        let (decision, reasons) = evaluate_governance_decision(0.8, 0.9, 1.0, &thresholds);
        assert_eq!(decision, GovernanceDecision::Ask);
        assert!(reasons.iter().any(|r| r.contains("R_high")));
    }

    #[test]
    fn decision_ask_low_confidence() {
        let thresholds = GovernanceThresholds::default();
        let (decision, reasons) = evaluate_governance_decision(0.2, 0.3, 1.0, &thresholds);
        assert_eq!(decision, GovernanceDecision::Ask);
        assert!(reasons.iter().any(|r| r.contains("C_min")));
    }

    #[test]
    fn decision_ask_low_coverage() {
        let thresholds = GovernanceThresholds::default();
        let (decision, reasons) = evaluate_governance_decision(0.2, 0.8, 0.3, &thresholds);
        assert_eq!(decision, GovernanceDecision::Ask);
        assert!(reasons.iter().any(|r| r.contains("coverage")));
    }

    #[test]
    fn decision_ask_gray_zone() {
        let thresholds = GovernanceThresholds::default();
        let (decision, reasons) = evaluate_governance_decision(0.5, 0.6, 0.8, &thresholds);
        assert_eq!(decision, GovernanceDecision::Ask);
        assert!(reasons.iter().any(|r| r.contains("gray_zone")));
    }

    #[test]
    fn fallback_assessment_max_conservative() {
        let a = build_fallback_governance_assessment("something broke");
        assert!((a.risk_score - 1.0).abs() < 1e-9);
        assert!(a.confidence.abs() < 1e-9);
        assert_eq!(a.decision, GovernanceDecision::Ask);
        assert!(a.reasons[0].contains("assessment_error"));
        assert!(a.reasons[0].contains("something broke"));
    }

    #[test]
    fn fallback_assessment_empty_error() {
        let a = build_fallback_governance_assessment("");
        assert!(a.reasons[0].contains("unknown"));
    }

    #[test]
    fn extract_paths_from_prompt() {
        let paths = extract_path_like_candidates("修改 src/main.rs 和 tests/lib.rs 文件");
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"tests/lib.rs".to_string()));
    }

    #[test]
    fn extract_paths_empty_prompt() {
        let paths = extract_path_like_candidates("  ");
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_paths_deduplication() {
        let paths = extract_path_like_candidates("foo.rs foo.rs foo.rs");
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn extract_tokens_from_prompt() {
        let tokens = extract_prompt_tokens("修改 auth_module 和 user_service");
        assert!(tokens.contains(&"auth_module".to_string()));
        assert!(tokens.contains(&"user_service".to_string()));
    }

    #[test]
    fn extract_tokens_capped_at_24() {
        let long_prompt = (0..50).map(|i| format!("token_{i}")).collect::<Vec<_>>().join(" ");
        let tokens = extract_prompt_tokens(&long_prompt);
        assert!(tokens.len() <= 24);
    }

    #[test]
    fn normalize_path_backslash() {
        assert_eq!(normalize_relative_path("src\\main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_path_dot_slash() {
        assert_eq!(normalize_relative_path("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn infer_module_first_segment() {
        assert_eq!(infer_module_from_path("src/lib.rs"), "src");
        assert_eq!(infer_module_from_path("crates/foo/bar.rs"), "crates");
    }

    #[test]
    fn infer_module_single_file() {
        assert_eq!(infer_module_from_path("main.rs"), "main.rs");
    }
}
