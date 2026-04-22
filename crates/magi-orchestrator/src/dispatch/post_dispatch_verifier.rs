use serde::{Deserialize, Serialize};

use super::batch::DispatchBatch;
use crate::verification_policy::VerificationMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSkippedReason {
    NoRunner,
    NoEntries,
    ExecutionFailed,
    NoChanges,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseVerificationStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseVerificationReport {
    pub status: BaseVerificationStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub criterion_id: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriteriaSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryVerificationOutcome {
    pub status: VerificationStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<VerificationSkippedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_verification: Option<BaseVerificationReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_results: Option<Vec<CriterionResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_summary: Option<CriteriaSummary>,
}

pub fn collect_batch_modified_files(batch: &DispatchBatch) -> Vec<String> {
    let mut files = std::collections::HashSet::new();
    for entry in batch.entries() {
        if let Some(ref result) = entry.result {
            if let Some(ref modified) = result.modified_files {
                for f in modified {
                    let normalized = f.trim().to_string();
                    if !normalized.is_empty() {
                        files.insert(normalized);
                    }
                }
            }
        }
    }
    files.into_iter().collect()
}

pub fn should_skip_verification(batch: &DispatchBatch) -> Option<VerificationSkippedReason> {
    let entries = batch.entries();
    if entries.is_empty() {
        return Some(VerificationSkippedReason::NoEntries);
    }

    let has_terminal_failure = entries
        .iter()
        .any(|e| matches!(e.status, super::batch::DispatchStatus::Failed | super::batch::DispatchStatus::Cancelled));
    if has_terminal_failure {
        return Some(VerificationSkippedReason::ExecutionFailed);
    }

    let modified_files = collect_batch_modified_files(batch);
    if modified_files.is_empty() {
        return Some(VerificationSkippedReason::NoChanges);
    }

    None
}

pub fn build_skipped_outcome(
    reason: VerificationSkippedReason,
    modified_files: Option<Vec<String>>,
) -> DeliveryVerificationOutcome {
    let summary = match reason {
        VerificationSkippedReason::NoRunner => "未配置验收执行器",
        VerificationSkippedReason::NoEntries => "未发现可验收的任务",
        VerificationSkippedReason::ExecutionFailed => "子任务失败或取消，跳过验收",
        VerificationSkippedReason::NoChanges => "未检测到文件修改，跳过验收",
    };
    DeliveryVerificationOutcome {
        status: VerificationStatus::Skipped,
        summary: summary.to_string(),
        details: None,
        skipped_reason: Some(reason),
        warnings: None,
        modified_files,
        base_verification: Some(BaseVerificationReport {
            status: BaseVerificationStatus::NotRun,
            summary: summary.to_string(),
            details: None,
            warnings: None,
        }),
        criteria_results: None,
        criteria_summary: None,
    }
}

pub fn compact_details(text: &str, limit: usize) -> Option<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return None;
    }
    if normalized.len() > limit {
        Some(format!("{}\n...(错误详情已截断)", &normalized[..limit]))
    } else {
        Some(normalized.to_string())
    }
}

pub fn build_criteria_summary(criteria_results: &[CriterionResult]) -> Option<CriteriaSummary> {
    if criteria_results.is_empty() {
        return None;
    }
    let passed = criteria_results
        .iter()
        .filter(|r| r.status == "passed")
        .count();
    Some(CriteriaSummary {
        total: criteria_results.len(),
        passed,
        failed: criteria_results.len() - passed,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissionContinuationPolicy {
    Stop,
    Auto,
}

pub fn resolve_delivery_continuation_policy(
    outcome_status: VerificationStatus,
    skipped_reason: Option<VerificationSkippedReason>,
    allow_deep_continuation: bool,
    verification_mode: VerificationMode,
) -> MissionContinuationPolicy {
    if verification_mode == VerificationMode::Smoke {
        return MissionContinuationPolicy::Stop;
    }
    if outcome_status == VerificationStatus::Passed {
        return MissionContinuationPolicy::Stop;
    }
    if outcome_status == VerificationStatus::Failed {
        return if allow_deep_continuation {
            MissionContinuationPolicy::Auto
        } else {
            MissionContinuationPolicy::Stop
        };
    }
    if skipped_reason == Some(VerificationSkippedReason::ExecutionFailed) {
        return if allow_deep_continuation {
            MissionContinuationPolicy::Auto
        } else {
            MissionContinuationPolicy::Stop
        };
    }
    MissionContinuationPolicy::Stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_details_truncates() {
        let long_text = "a".repeat(4000);
        let result = compact_details(&long_text, 3000).unwrap();
        assert!(result.contains("已截断"));
        assert!(result.len() < 4000);
    }

    #[test]
    fn compact_details_empty() {
        assert!(compact_details("", 3000).is_none());
        assert!(compact_details("  ", 3000).is_none());
    }

    #[test]
    fn compact_details_short() {
        let result = compact_details("hello", 3000).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn build_criteria_summary_empty() {
        assert!(build_criteria_summary(&[]).is_none());
    }

    #[test]
    fn build_criteria_summary_mixed() {
        let results = vec![
            CriterionResult {
                criterion_id: "c1".to_string(),
                status: "passed".to_string(),
                detail: "ok".to_string(),
            },
            CriterionResult {
                criterion_id: "c2".to_string(),
                status: "failed".to_string(),
                detail: "err".to_string(),
            },
        ];
        let summary = build_criteria_summary(&results).unwrap();
        assert_eq!(summary.total, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn continuation_policy_smoke_always_stops() {
        assert_eq!(
            resolve_delivery_continuation_policy(
                VerificationStatus::Failed,
                None,
                true,
                VerificationMode::Smoke,
            ),
            MissionContinuationPolicy::Stop
        );
    }

    #[test]
    fn continuation_policy_passed_stops() {
        assert_eq!(
            resolve_delivery_continuation_policy(
                VerificationStatus::Passed,
                None,
                true,
                VerificationMode::Standard,
            ),
            MissionContinuationPolicy::Stop
        );
    }

    #[test]
    fn continuation_policy_failed_with_deep() {
        assert_eq!(
            resolve_delivery_continuation_policy(
                VerificationStatus::Failed,
                None,
                true,
                VerificationMode::Standard,
            ),
            MissionContinuationPolicy::Auto
        );
    }

    #[test]
    fn continuation_policy_failed_without_deep() {
        assert_eq!(
            resolve_delivery_continuation_policy(
                VerificationStatus::Failed,
                None,
                false,
                VerificationMode::Standard,
            ),
            MissionContinuationPolicy::Stop
        );
    }

    #[test]
    fn skipped_outcome_has_correct_status() {
        let outcome = build_skipped_outcome(VerificationSkippedReason::NoRunner, None);
        assert_eq!(outcome.status, VerificationStatus::Skipped);
        assert_eq!(outcome.skipped_reason, Some(VerificationSkippedReason::NoRunner));
    }

    #[test]
    fn skip_check_no_entries() {
        let batch = super::super::batch::DispatchBatch::new(Some("test"));
        assert_eq!(
            should_skip_verification(&batch),
            Some(VerificationSkippedReason::NoEntries)
        );
    }
}
