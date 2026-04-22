use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPath {
    DirectResponse,
    LightweightAnalysis,
    TaskExecution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Direct,
    Analysis,
    Dispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    Standard,
    Deep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelAutonomyCapability {
    C0,
    C1,
    C2,
    C3,
}

pub struct ModelCapabilityInput {
    pub model: String,
    pub enable_thinking: bool,
    pub reasoning_effort: Option<String>,
    pub autonomy_capability: Option<ModelAutonomyCapability>,
}

pub struct EffectiveModeInput {
    pub planning_mode: PlanMode,
    pub model_capability: Option<ModelAutonomyCapability>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug)]
pub struct RequestClassification {
    pub entry_path: EntryPath,
    pub requires_modification: bool,
    pub has_write_intent: bool,
    pub has_high_impact_intent: bool,
    pub decision_factors: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct RequirementAnalysis {
    pub goal: String,
    pub analysis: String,
    pub constraints: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub risk_level: RiskLevel,
    pub risk_factors: Vec<String>,
    pub entry_path: EntryPath,
    pub execution_mode: Option<ExecutionMode>,
    pub requires_modification: bool,
    pub decision_factors: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct EntryRoutingDecision {
    pub requires_modification: bool,
    pub execution_mode: Option<ExecutionMode>,
    pub entry_path: EntryPath,
    pub decision_factors: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct EffectiveModeResolution {
    pub planning_mode: PlanMode,
    pub requested_planning_mode: PlanMode,
    pub model_capability: ModelAutonomyCapability,
    pub allow_deep_continuation: bool,
    pub allow_auto_governance_resume: bool,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OrchestrationEntryResolution {
    pub requested_planning_mode: PlanMode,
    pub effective_mode: EffectiveModeResolution,
    pub classification: RequestClassification,
    pub requirement_analysis: RequirementAnalysis,
    pub routing_decision: EntryRoutingDecision,
}

const DIRECT_RESPONSE_PATTERNS: &[&str] = &[
    "什么是", "怎么", "如何", "解释", "说明", "是什么",
    "what is", "how to", "explain", "describe", "tell me",
    "why", "为什么", "哪个", "which",
];

const WRITE_INTENT_PATTERNS: &[&str] = &[
    "修改", "添加", "删除", "创建", "实现", "写", "更新", "重构", "修复",
    "fix", "add", "create", "implement", "write", "update", "refactor",
    "remove", "delete", "change", "移除", "改",
];

const HIGH_IMPACT_PATTERNS: &[&str] = &[
    "重写", "迁移", "重构", "架构", "全局", "所有",
    "rewrite", "migrate", "architecture", "global", "all files",
];

const ANALYSIS_PATTERNS: &[&str] = &[
    "分析", "查看", "检查", "审查", "review", "analyze", "check",
    "inspect", "look at", "读", "read", "show me", "给我看",
    "list", "列出", "find", "查找", "搜索", "search",
];

fn contains_any_ci(text: &str, patterns: &[&str]) -> bool {
    let lower = text.to_lowercase();
    patterns.iter().any(|p| lower.contains(&p.to_lowercase()))
}

pub fn classify_request(prompt: &str, planning_mode: PlanMode) -> RequestClassification {
    let trimmed = prompt.trim();
    let has_write_intent = contains_any_ci(trimmed, WRITE_INTENT_PATTERNS);
    let has_high_impact = contains_any_ci(trimmed, HIGH_IMPACT_PATTERNS);
    let has_analysis_intent = contains_any_ci(trimmed, ANALYSIS_PATTERNS);
    let has_question_intent = contains_any_ci(trimmed, DIRECT_RESPONSE_PATTERNS);

    let mut factors = Vec::new();
    let (entry_path, reason) = if !has_write_intent && has_question_intent && trimmed.len() < 200 {
        factors.push("纯提问意图".to_string());
        (
            EntryPath::DirectResponse,
            "用户提问，无需修改代码".to_string(),
        )
    } else if !has_write_intent && has_analysis_intent {
        factors.push("只读分析意图".to_string());
        (
            EntryPath::LightweightAnalysis,
            "用户请求分析/查看，无需修改".to_string(),
        )
    } else {
        if has_write_intent {
            factors.push("包含写操作意图".to_string());
        }
        if has_high_impact {
            factors.push("高影响改动".to_string());
        }
        if planning_mode == PlanMode::Deep {
            factors.push("deep 规划模式".to_string());
        }
        (
            EntryPath::TaskExecution,
            "需要执行任务".to_string(),
        )
    };

    RequestClassification {
        entry_path,
        requires_modification: has_write_intent,
        has_write_intent,
        has_high_impact_intent: has_high_impact,
        decision_factors: factors,
        reason,
    }
}

const HIGH_AUTONOMY_HINTS: &[&str] = &[
    "gpt-5", "o3", "o4", "claude-4", "opus-4", "sonnet-4", "gemini-2.5", "gemini 2.5",
];

const DEEP_PLANNING_HINTS: &[&str] = &[
    "claude-3.7", "claude-3.5", "sonnet", "opus", "gpt-4.1", "gpt-4o", "gemini-1.5", "gemini-2.0",
];

pub fn resolve_model_autonomy_capability(input: &ModelCapabilityInput) -> ModelAutonomyCapability {
    if let Some(explicit) = input.autonomy_capability {
        return explicit;
    }

    let model = input.model.to_lowercase();
    let reasoning_effort = input
        .reasoning_effort
        .as_deref()
        .unwrap_or("medium");

    if input.enable_thinking
        || reasoning_effort == "high"
        || reasoning_effort == "xhigh"
        || HIGH_AUTONOMY_HINTS.iter().any(|h| model.contains(h))
    {
        return ModelAutonomyCapability::C3;
    }

    if reasoning_effort == "medium"
        || DEEP_PLANNING_HINTS.iter().any(|h| model.contains(h))
    {
        return ModelAutonomyCapability::C2;
    }

    ModelAutonomyCapability::C1
}

pub fn resolve_effective_mode(input: EffectiveModeInput) -> EffectiveModeResolution {
    let model_capability = input.model_capability.unwrap_or(ModelAutonomyCapability::C3);
    let allows_deep = matches!(model_capability, ModelAutonomyCapability::C1 | ModelAutonomyCapability::C3);
    let requested_deep = input.planning_mode == PlanMode::Deep;
    let planning_mode = if requested_deep && allows_deep {
        PlanMode::Deep
    } else {
        PlanMode::Standard
    };
    let degraded = requested_deep && !allows_deep;

    EffectiveModeResolution {
        planning_mode,
        requested_planning_mode: input.planning_mode,
        model_capability,
        allow_deep_continuation: planning_mode == PlanMode::Deep,
        allow_auto_governance_resume: true,
        degraded,
        degraded_reason: if degraded {
            Some(format!(
                "当前模型自治能力为 {:?}，Deep 模式要求 C1 或 C3，已自动降级为 Standard 模式",
                model_capability
            ))
        } else {
            None
        },
    }
}

fn extract_primary_intent(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();
    if first_line.len() > 120 {
        format!("{}...", &first_line[..120])
    } else {
        first_line.to_string()
    }
}

fn extract_user_constraints(prompt: &str) -> Vec<String> {
    let constraint_patterns = [
        "不要", "不能", "禁止", "必须", "确保", "注意",
        "don't", "must", "should not", "ensure",
    ];
    prompt
        .lines()
        .filter(|line| {
            let lower = line.to_lowercase();
            constraint_patterns.iter().any(|p| lower.contains(p))
        })
        .map(|line| line.trim().to_string())
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .take(10)
        .collect()
}

fn assess_risk(
    prompt: &str,
    planning_mode: PlanMode,
    classification: &RequestClassification,
    constraints: &[String],
) -> (RiskLevel, Vec<String>) {
    let mut score = 0u32;
    let mut factors = Vec::new();

    if planning_mode == PlanMode::Deep && classification.entry_path == EntryPath::TaskExecution {
        score += 2;
        factors.push("任务运行在 deep 规划模式".to_string());
    }
    if classification.has_high_impact_intent {
        score += 2;
        factors.push("需求涉及高影响改动".to_string());
    } else if classification.has_write_intent {
        score += 1;
        factors.push("需求包含代码修改".to_string());
    }
    if prompt.len() >= 280 {
        score += 1;
        factors.push("需求描述较长".to_string());
    }
    if constraints.len() >= 3 {
        score += 1;
        factors.push("用户约束较多".to_string());
    }

    let level = if score >= 4 {
        RiskLevel::High
    } else if score >= 2 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    (level, factors)
}

pub fn build_requirement_analysis(
    prompt: &str,
    planning_mode: PlanMode,
    classification: &RequestClassification,
) -> RequirementAnalysis {
    let goal = extract_primary_intent(prompt);
    let constraints = extract_user_constraints(prompt);
    let (risk_level, risk_factors) = assess_risk(prompt, planning_mode, classification, &constraints);

    let execution_mode = match classification.entry_path {
        EntryPath::DirectResponse => Some(ExecutionMode::Direct),
        EntryPath::LightweightAnalysis => Some(ExecutionMode::Analysis),
        EntryPath::TaskExecution => None,
    };

    let analysis = format!(
        "围绕「{}」{}；风险等级为 {:?}",
        goal,
        match classification.entry_path {
            EntryPath::DirectResponse => "直接回答用户问题",
            EntryPath::LightweightAnalysis => "进行只读分析",
            EntryPath::TaskExecution => "建立执行计划",
        },
        risk_level
    );

    RequirementAnalysis {
        goal,
        analysis,
        constraints,
        acceptance_criteria: Vec::new(),
        risk_level,
        risk_factors,
        entry_path: classification.entry_path,
        execution_mode,
        requires_modification: classification.requires_modification,
        decision_factors: classification.decision_factors.clone(),
        reason: classification.reason.clone(),
    }
}

pub fn resolve_orchestration_entry(
    prompt: &str,
    requested_planning_mode: PlanMode,
    model_capability: Option<ModelAutonomyCapability>,
) -> OrchestrationEntryResolution {
    let effective_mode = resolve_effective_mode(EffectiveModeInput {
        planning_mode: requested_planning_mode,
        model_capability,
    });
    let classification = classify_request(prompt, effective_mode.planning_mode);
    let requirement_analysis =
        build_requirement_analysis(prompt, effective_mode.planning_mode, &classification);

    let routing_decision = EntryRoutingDecision {
        requires_modification: requirement_analysis.requires_modification,
        execution_mode: requirement_analysis.execution_mode,
        entry_path: requirement_analysis.entry_path,
        decision_factors: requirement_analysis.decision_factors.clone(),
        reason: requirement_analysis.reason.clone(),
    };

    OrchestrationEntryResolution {
        requested_planning_mode,
        effective_mode,
        classification,
        requirement_analysis,
        routing_decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_routes_to_direct_response() {
        let result = resolve_orchestration_entry("什么是 Rust 的所有权系统？", PlanMode::Standard, None);
        assert_eq!(result.routing_decision.entry_path, EntryPath::DirectResponse);
        assert!(!result.routing_decision.requires_modification);
    }

    #[test]
    fn analysis_request_routes_to_lightweight() {
        let result = resolve_orchestration_entry("分析一下这个模块的性能瓶颈", PlanMode::Standard, None);
        assert_eq!(result.routing_decision.entry_path, EntryPath::LightweightAnalysis);
    }

    #[test]
    fn write_intent_routes_to_task_execution() {
        let result = resolve_orchestration_entry("修改 auth 模块，添加 JWT 支持", PlanMode::Standard, None);
        assert_eq!(result.routing_decision.entry_path, EntryPath::TaskExecution);
        assert!(result.routing_decision.requires_modification);
    }

    #[test]
    fn high_impact_detected() {
        let classification = classify_request("重写整个认证架构", PlanMode::Standard);
        assert!(classification.has_high_impact_intent);
        assert!(classification.has_write_intent);
    }

    #[test]
    fn deep_mode_increases_risk() {
        let result = resolve_orchestration_entry("实现用户登录功能", PlanMode::Deep, None);
        assert!(result.effective_mode.allow_deep_continuation);
        assert!(result.requirement_analysis.risk_factors.iter().any(|f| f.contains("deep")));
    }

    #[test]
    fn constraints_extracted() {
        let analysis = build_requirement_analysis(
            "修改配置文件\n不要删除现有配置\n必须保持向后兼容",
            PlanMode::Standard,
            &classify_request("修改配置文件\n不要删除现有配置\n必须保持向后兼容", PlanMode::Standard),
        );
        assert!(analysis.constraints.len() >= 2);
    }

    #[test]
    fn long_prompt_increases_risk() {
        let long_prompt = format!("实现一个复杂功能 {}", "详细描述 ".repeat(50));
        let classification = classify_request(&long_prompt, PlanMode::Standard);
        let (risk, _) = assess_risk(&long_prompt, PlanMode::Standard, &classification, &[]);
        assert!(matches!(risk, RiskLevel::Medium | RiskLevel::High));
    }

    #[test]
    fn model_capability_c3_from_thinking() {
        let cap = resolve_model_autonomy_capability(&ModelCapabilityInput {
            model: "claude-3.5-sonnet".to_string(),
            enable_thinking: true,
            reasoning_effort: None,
            autonomy_capability: None,
        });
        assert_eq!(cap, ModelAutonomyCapability::C3);
    }

    #[test]
    fn model_capability_c3_from_high_autonomy_model() {
        let cap = resolve_model_autonomy_capability(&ModelCapabilityInput {
            model: "claude-4-opus".to_string(),
            enable_thinking: false,
            reasoning_effort: None,
            autonomy_capability: None,
        });
        assert_eq!(cap, ModelAutonomyCapability::C3);
    }

    #[test]
    fn model_capability_c2_from_medium_effort() {
        let cap = resolve_model_autonomy_capability(&ModelCapabilityInput {
            model: "some-model".to_string(),
            enable_thinking: false,
            reasoning_effort: Some("medium".to_string()),
            autonomy_capability: None,
        });
        assert_eq!(cap, ModelAutonomyCapability::C2);
    }

    #[test]
    fn model_capability_c1_fallback() {
        let cap = resolve_model_autonomy_capability(&ModelCapabilityInput {
            model: "llama-3".to_string(),
            enable_thinking: false,
            reasoning_effort: Some("low".to_string()),
            autonomy_capability: None,
        });
        assert_eq!(cap, ModelAutonomyCapability::C1);
    }

    #[test]
    fn model_capability_explicit_override() {
        let cap = resolve_model_autonomy_capability(&ModelCapabilityInput {
            model: "gpt-5".to_string(),
            enable_thinking: true,
            reasoning_effort: Some("high".to_string()),
            autonomy_capability: Some(ModelAutonomyCapability::C0),
        });
        assert_eq!(cap, ModelAutonomyCapability::C0);
    }

    #[test]
    fn deep_mode_degrades_for_c2() {
        let resolution = resolve_effective_mode(EffectiveModeInput {
            planning_mode: PlanMode::Deep,
            model_capability: Some(ModelAutonomyCapability::C2),
        });
        assert!(resolution.degraded);
        assert_eq!(resolution.planning_mode, PlanMode::Standard);
        assert!(!resolution.allow_deep_continuation);
        assert!(resolution.degraded_reason.is_some());
    }

    #[test]
    fn deep_mode_preserved_for_c3() {
        let resolution = resolve_effective_mode(EffectiveModeInput {
            planning_mode: PlanMode::Deep,
            model_capability: Some(ModelAutonomyCapability::C3),
        });
        assert!(!resolution.degraded);
        assert_eq!(resolution.planning_mode, PlanMode::Deep);
        assert!(resolution.allow_deep_continuation);
    }

    #[test]
    fn deep_mode_preserved_for_c1() {
        let resolution = resolve_effective_mode(EffectiveModeInput {
            planning_mode: PlanMode::Deep,
            model_capability: Some(ModelAutonomyCapability::C1),
        });
        assert!(!resolution.degraded);
        assert_eq!(resolution.planning_mode, PlanMode::Deep);
    }

    #[test]
    fn standard_mode_never_degrades() {
        let resolution = resolve_effective_mode(EffectiveModeInput {
            planning_mode: PlanMode::Standard,
            model_capability: Some(ModelAutonomyCapability::C0),
        });
        assert!(!resolution.degraded);
        assert_eq!(resolution.planning_mode, PlanMode::Standard);
    }

    #[test]
    fn default_model_capability_is_c3() {
        let resolution = resolve_effective_mode(EffectiveModeInput {
            planning_mode: PlanMode::Deep,
            model_capability: None,
        });
        assert_eq!(resolution.model_capability, ModelAutonomyCapability::C3);
        assert!(!resolution.degraded);
    }
}
