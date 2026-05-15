//! Task System v2 — Tier 4 / L19 ValidationRunner：Plan 节点的"是否真做完"档案。
//!
//! 架构定义（参见 docs/task-system-v2/01-architecture.md L19）：
//! 验证子系统覆盖 **测试套件 / 类型检查 / 集成 smoke / 性能基准** 四类信号。Runner 是
//! 一个 `Task`（L11，TaskKind::Validation），由 Coordinator 调度；每次 Plan 节点完成
//! 触发 Runner，结果写回 Plan 节点。**没有 Runner 通过的 Plan 节点不算完成**——
//! 避免"模型说做完了"的认知偏差。
//!
//! 本 crate 只承担"记录与查询"职责：
//! - 把验证结果按 (plan_step_id, kind) upsert 到 mission 级 ValidationReport
//! - 渲染 prompt 段落给 Coordinator 看当前 mission 的验证现状
//! - 暴露 `validation_record` 工具入参解析
//!
//! 真正去跑 cargo test / tsc / smoke 的活，由 Validation 任务通过 shell_exec 等基础
//! 工具完成；本 crate 不替它跑命令，只回填结论——保持职责单一、与 Plan 解耦。
//!
//! 物理存储：`~/.magi/projects/{slug}/missions/{mission_id}/validation.md`。
//! 单 mission 单文档，frontmatter 描述元信息，body 用 JSON-lines 记录每条 record。
//! 这样既可 grep / diff，又能 round-trip 无损——与同 Tier 的 KnowledgeGraph 同构。

use magi_core::{MissionId, UtcMillis, WorkspaceRootPath};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// ValidationKind / ValidationOutcome
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationKind {
    /// 测试套件：cargo test / npm test / pytest 等。
    TestSuite,
    /// 类型检查：tsc / mypy / cargo check 等。
    TypeCheck,
    /// 集成 smoke：跨进程 / 跨服务的端到端验证。
    IntegrationSmoke,
    /// 性能基准：benchmark / 压测。
    Benchmark,
}

impl ValidationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TestSuite => "test_suite",
            Self::TypeCheck => "type_check",
            Self::IntegrationSmoke => "integration_smoke",
            Self::Benchmark => "benchmark",
        }
    }

    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "test_suite" | "test" | "tests" | "unit" | "unit_test" => Some(Self::TestSuite),
            "type_check" | "typecheck" | "types" | "lint" => Some(Self::TypeCheck),
            "integration_smoke" | "integration" | "smoke" | "e2e" => Some(Self::IntegrationSmoke),
            "benchmark" | "bench" | "perf" => Some(Self::Benchmark),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    Pass,
    Fail,
    Skipped,
}

impl ValidationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }

    pub fn from_str_lenient(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pass" | "passed" | "ok" | "success" => Some(Self::Pass),
            "fail" | "failed" | "error" | "ng" => Some(Self::Fail),
            "skip" | "skipped" | "n/a" | "na" => Some(Self::Skipped),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ValidationRecord / ValidationReport
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRecord {
    pub plan_step_id: String,
    pub kind: ValidationKind,
    pub outcome: ValidationOutcome,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
    pub recorded_at: UtcMillis,
    /// 同 (step, kind) 每次 upsert 自增；用于辨认"是否补跑过"。
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub mission_id: MissionId,
    pub records: Vec<ValidationRecord>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl ValidationReport {
    pub fn new(mission_id: MissionId, now: UtcMillis) -> Self {
        Self {
            mission_id,
            records: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn find_mut(
        &mut self,
        step_id: &str,
        kind: ValidationKind,
    ) -> Option<&mut ValidationRecord> {
        self.records
            .iter_mut()
            .find(|r| r.plan_step_id == step_id && r.kind == kind)
    }

    /// 步骤是否被认定"已通过完整验证"：至少有一条 outcome=Pass，且没有未消解的 Fail。
    /// Coordinator 决定一个 Plan 节点是否可以从 InProgress 跳到 Completed 时调用。
    pub fn step_is_passing(&self, step_id: &str) -> bool {
        let mut saw_pass = false;
        for r in self.records.iter().filter(|r| r.plan_step_id == step_id) {
            match r.outcome {
                ValidationOutcome::Fail => return false,
                ValidationOutcome::Pass => saw_pass = true,
                ValidationOutcome::Skipped => {}
            }
        }
        saw_pass
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,
    #[error("validation 数据缺失或非法：{reason}")]
    InvalidRecord { reason: String },
    #[error("validation IO 失败 (path={path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct ValidationStore {
    root: PathBuf,
}

impl ValidationStore {
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, ValidationError> {
        let home = dirs_home()?;
        Self::open_with_home(&home, workspace_root)
    }

    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, ValidationError> {
        let slug = workspace_slug(workspace_root.as_str());
        let root = magi_home.join("projects").join(slug).join("missions");
        fs::create_dir_all(&root).map_err(|source| ValidationError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    fn report_path(&self, mission_id: &MissionId) -> PathBuf {
        self.root.join(mission_id.as_str()).join("validation.md")
    }

    pub fn load(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<ValidationReport>, ValidationError> {
        let path = self.report_path(mission_id);
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(ValidationError::Io { path, source }),
        };
        parse_report(&raw).map(Some)
    }

    pub fn save(&self, report: &ValidationReport) -> Result<(), ValidationError> {
        let path = self.report_path(&report.mission_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ValidationError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered = render_report(report);
        fs::write(&path, rendered).map_err(|source| ValidationError::Io { path, source })
    }

    /// 为 system prompt 渲染 Validation 段落。空报告返回 None，避免噪音注入。
    pub fn render_for_prompt(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<String>, ValidationError> {
        let Some(report) = self.load(mission_id)? else {
            return Ok(None);
        };
        if report.records.is_empty() {
            return Ok(None);
        }
        let mut out = String::new();
        out.push_str("# Mission Validation\n\n");
        out.push_str(&format!("- mission_id: {}\n", report.mission_id.as_str()));
        out.push_str(&format!("- total_records: {}\n\n", report.records.len()));

        // 按 plan_step_id 聚合，便于 Coordinator 一眼对应到 Plan 节点。
        let mut by_step: HashMap<&str, Vec<&ValidationRecord>> = HashMap::new();
        for record in &report.records {
            by_step
                .entry(record.plan_step_id.as_str())
                .or_default()
                .push(record);
        }
        let mut step_ids: Vec<&str> = by_step.keys().copied().collect();
        step_ids.sort();
        for step_id in step_ids {
            let bucket = &by_step[step_id];
            out.push_str(&format!("## Step `{}`\n", step_id));
            for r in bucket {
                out.push_str(&format!(
                    "- {} → **{}**",
                    r.kind.as_str(),
                    r.outcome.as_str(),
                ));
                if let Some(command) = &r.command {
                    if !command.is_empty() {
                        out.push_str(&format!(" (cmd: `{command}`)"));
                    }
                }
                if let Some(evidence) = &r.evidence {
                    if !evidence.is_empty() {
                        out.push_str(&format!(" — {evidence}"));
                    }
                }
                out.push('\n');
            }
            out.push('\n');
        }
        Ok(Some(out))
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// 进程级缓存，按 workspace_root 聚合 ValidationStore。HOME 不可用时退到
/// `$TMPDIR/magi-validation-runner`，与同 Tier 其它 crate 行为一致。
pub struct ValidationRunnerRegistry {
    inner: RwLock<HashMap<String, Arc<ValidationStore>>>,
    fallback_home: PathBuf,
}

impl Default for ValidationRunnerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ValidationRunnerRegistry {
    pub fn new() -> Self {
        let fallback_home = std::env::temp_dir().join("magi-validation-runner");
        let _ = fs::create_dir_all(&fallback_home);
        Self {
            inner: RwLock::new(HashMap::new()),
            fallback_home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<ValidationStore>, ValidationError> {
        let key = workspace_root.as_str().to_string();
        if let Some(store) = self
            .inner
            .read()
            .expect("validation registry poisoned")
            .get(&key)
        {
            return Ok(store.clone());
        }
        let store = match ValidationStore::open(workspace_root) {
            Ok(store) => store,
            Err(ValidationError::HomeDirUnavailable) => {
                ValidationStore::open_with_home(&self.fallback_home, workspace_root)?
            }
            Err(err) => return Err(err),
        };
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("validation registry poisoned")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

// ---------------------------------------------------------------------------
// Tool argument parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ValidationRecordArgs {
    pub plan_step_id: String,
    pub kind: ValidationKind,
    pub outcome: ValidationOutcome,
    pub command: Option<String>,
    pub evidence: Option<String>,
}

pub fn parse_validation_record_arguments(
    raw: &serde_json::Value,
) -> Result<ValidationRecordArgs, ValidationError> {
    let obj = raw
        .as_object()
        .ok_or_else(|| ValidationError::InvalidRecord {
            reason: "arguments 必须为对象".to_string(),
        })?;
    let plan_step_id = obj
        .get("plan_step_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ValidationError::InvalidRecord {
            reason: "缺少 plan_step_id 字段".to_string(),
        })?
        .trim()
        .to_string();
    if plan_step_id.is_empty() {
        return Err(ValidationError::InvalidRecord {
            reason: "plan_step_id 不能为空".to_string(),
        });
    }
    let kind_raw = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ValidationError::InvalidRecord {
            reason: "缺少 kind 字段（test_suite/type_check/integration_smoke/benchmark）"
                .to_string(),
        })?;
    let kind = ValidationKind::from_str_lenient(kind_raw).ok_or_else(|| {
        ValidationError::InvalidRecord {
            reason: format!("kind 非法：{kind_raw}"),
        }
    })?;
    let outcome_raw = obj
        .get("outcome")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ValidationError::InvalidRecord {
            reason: "缺少 outcome 字段（pass/fail/skipped）".to_string(),
        })?;
    let outcome = ValidationOutcome::from_str_lenient(outcome_raw).ok_or_else(|| {
        ValidationError::InvalidRecord {
            reason: format!("outcome 非法：{outcome_raw}"),
        }
    })?;
    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let evidence = obj
        .get("evidence")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(ValidationRecordArgs {
        plan_step_id,
        kind,
        outcome,
        command,
        evidence,
    })
}

/// 应用 validation_record 入参到 report：按 (plan_step_id, kind) upsert。
/// 同信息再写不视为变化（避免无意义磁盘抖动 + version 漂高）。
pub fn apply_validation_record(
    report: &mut ValidationReport,
    args: ValidationRecordArgs,
    now: UtcMillis,
) -> bool {
    if let Some(existing) = report.find_mut(&args.plan_step_id, args.kind) {
        let same = existing.outcome == args.outcome
            && existing.command == args.command
            && existing.evidence == args.evidence;
        if same {
            return false;
        }
        existing.outcome = args.outcome;
        existing.command = args.command;
        existing.evidence = args.evidence;
        existing.recorded_at = now;
        existing.version = existing.version.saturating_add(1);
    } else {
        report.records.push(ValidationRecord {
            plan_step_id: args.plan_step_id,
            kind: args.kind,
            outcome: args.outcome,
            command: args.command,
            evidence: args.evidence,
            recorded_at: now,
            version: 1,
        });
    }
    report.updated_at = now;
    true
}

// ---------------------------------------------------------------------------
// 序列化 / 反序列化（frontmatter + JSON-lines body）
// ---------------------------------------------------------------------------

fn render_report(report: &ValidationReport) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("mission_id: {}\n", report.mission_id.as_str()));
    out.push_str(&format!("created_at: {}\n", report.created_at.0));
    out.push_str(&format!("updated_at: {}\n", report.updated_at.0));
    out.push_str(&format!("record_count: {}\n", report.records.len()));
    out.push_str("---\n\n");
    out.push_str("## Records\n");
    for record in &report.records {
        let line = serde_json::to_string(record).expect("ValidationRecord 序列化必须成功");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn parse_report(raw: &str) -> Result<ValidationReport, ValidationError> {
    let body_start = raw
        .strip_prefix("---\n")
        .ok_or_else(|| ValidationError::InvalidRecord {
            reason: "缺少 frontmatter 起始 ---".to_string(),
        })?;
    let (front, body) =
        body_start
            .split_once("\n---\n")
            .ok_or_else(|| ValidationError::InvalidRecord {
                reason: "缺少 frontmatter 结束 ---".to_string(),
            })?;
    let mut mission_id: Option<MissionId> = None;
    let mut created_at: Option<u64> = None;
    let mut updated_at: Option<u64> = None;
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "mission_id" => mission_id = Some(MissionId::new(value.to_string())),
            "created_at" => {
                created_at = Some(value.parse().map_err(|_| ValidationError::InvalidRecord {
                    reason: format!("created_at 解析失败：{value}"),
                })?)
            }
            "updated_at" => {
                updated_at = Some(value.parse().map_err(|_| ValidationError::InvalidRecord {
                    reason: format!("updated_at 解析失败：{value}"),
                })?)
            }
            _ => {}
        }
    }
    let mission_id = mission_id.ok_or_else(|| ValidationError::InvalidRecord {
        reason: "mission_id 缺失".to_string(),
    })?;
    let created_at = UtcMillis(created_at.unwrap_or(0));
    let updated_at = UtcMillis(updated_at.unwrap_or(created_at.0));

    let mut records = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("##") {
            continue;
        }
        let record: ValidationRecord =
            serde_json::from_str(trimmed).map_err(|err| ValidationError::InvalidRecord {
                reason: format!("record 行解析失败：{err} ({trimmed})"),
            })?;
        records.push(record);
    }

    Ok(ValidationReport {
        mission_id,
        records,
        created_at,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn dirs_home() -> Result<PathBuf, ValidationError> {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(ValidationError::HomeDirUnavailable)?;
    Ok(base.join(".magi"))
}

fn workspace_slug(workspace_root: &str) -> String {
    let trimmed = workspace_root.trim_start_matches('/').trim_end_matches('/');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        format!("-{}", trimmed.replace('/', "-"))
    }
}

// ---------------------------------------------------------------------------
// Tool entry：`validation_record` 工具执行体
// ---------------------------------------------------------------------------

/// S15 工具下沉：`validation_record` 完整执行体收口在本 crate。`(plan_step_id, kind)`
/// 唯一，重复写入按 upsert 处理并 bump version。
pub fn execute_validation_record_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    store: Option<&ValidationStore>,
    session_id: &magi_core::SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    task_id: &magi_core::TaskId,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
    use magi_event_bus::{EventContext, EventEnvelope};
    let Some(store) = store else {
        return (
            serde_json::json!({
                "tool": "validation_record",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission validation runner",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match parse_validation_record_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = UtcMillis::now();
    let mut report = match store.load(mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => ValidationReport::new(mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let plan_step_id = args.plan_step_id.clone();
    let kind = args.kind;
    let outcome = args.outcome;
    let changed = apply_validation_record(&mut report, args, now);
    if let Err(err) = store.save(&report) {
        return (
            serde_json::json!({
                "tool": "validation_record",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let step_passing = report.step_is_passing(&plan_step_id);
    let payload = serde_json::json!({
        "tool": "validation_record",
        "status": "succeeded",
        "mission_id": report.mission_id.to_string(),
        "plan_step_id": plan_step_id,
        "kind": kind.as_str(),
        "outcome": outcome.as_str(),
        "record_count": report.records.len(),
        "changed": changed,
        "step_is_passing": step_passing,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-validation-updated-{}", UtcMillis::now().0)),
            "task.validation.updated",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": report.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "plan_step_id": plan_step_id,
                "kind": kind.as_str(),
                "outcome": outcome.as_str(),
                "changed": changed,
                "step_is_passing": step_passing,
                "record_count": report.records.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> MissionId {
        MissionId::new("mission-validation-test".to_string())
    }

    #[test]
    fn upsert_creates_then_increments_version() {
        let mut report = ValidationReport::new(mission(), UtcMillis(1000));
        let changed = apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Pass,
                command: Some("cargo test -p magi-api".to_string()),
                evidence: Some("305 passed".to_string()),
            },
            UtcMillis(1100),
        );
        assert!(changed);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].version, 1);

        // 同内容再写应当无变化。
        let changed2 = apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Pass,
                command: Some("cargo test -p magi-api".to_string()),
                evidence: Some("305 passed".to_string()),
            },
            UtcMillis(1200),
        );
        assert!(!changed2);
        assert_eq!(report.records[0].version, 1);

        // 改 outcome（例如复跑由 Pass 变 Fail）后 version+1。
        let changed3 = apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Fail,
                command: Some("cargo test -p magi-api".to_string()),
                evidence: Some("regression in foo".to_string()),
            },
            UtcMillis(1300),
        );
        assert!(changed3);
        assert_eq!(report.records[0].version, 2);
        assert_eq!(report.records[0].outcome, ValidationOutcome::Fail);
    }

    #[test]
    fn upsert_distinguishes_kind_namespace() {
        let mut report = ValidationReport::new(mission(), UtcMillis(0));
        for kind in [
            ValidationKind::TestSuite,
            ValidationKind::TypeCheck,
            ValidationKind::IntegrationSmoke,
            ValidationKind::Benchmark,
        ] {
            apply_validation_record(
                &mut report,
                ValidationRecordArgs {
                    plan_step_id: "s1".to_string(),
                    kind,
                    outcome: ValidationOutcome::Pass,
                    command: None,
                    evidence: None,
                },
                UtcMillis(1),
            );
        }
        assert_eq!(
            report.records.len(),
            4,
            "同 step 下不同 kind 必须独立存在"
        );
    }

    #[test]
    fn step_is_passing_requires_pass_and_no_fail() {
        let mut report = ValidationReport::new(mission(), UtcMillis(0));
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Pass,
                command: None,
                evidence: None,
            },
            UtcMillis(1),
        );
        assert!(report.step_is_passing("s1"));

        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TypeCheck,
                outcome: ValidationOutcome::Fail,
                command: None,
                evidence: Some("type error".to_string()),
            },
            UtcMillis(2),
        );
        assert!(
            !report.step_is_passing("s1"),
            "出现 Fail 后步骤必须视为未通过"
        );

        // s2 还没记录过，必须返回 false：无 Pass = 未通过。
        assert!(!report.step_is_passing("s2"));

        // 全部都是 Skipped 同样不算通过。
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s3".to_string(),
                kind: ValidationKind::Benchmark,
                outcome: ValidationOutcome::Skipped,
                command: None,
                evidence: None,
            },
            UtcMillis(3),
        );
        assert!(!report.step_is_passing("s3"));
    }

    #[test]
    fn parse_args_validates_required_fields() {
        let err = parse_validation_record_arguments(
            &serde_json::json!({"kind": "test_suite", "outcome": "pass"}),
        )
        .expect_err("缺 plan_step_id 必须报错");
        assert!(matches!(err, ValidationError::InvalidRecord { .. }));

        let err = parse_validation_record_arguments(&serde_json::json!({
            "plan_step_id": "s1",
            "kind": "wrong",
            "outcome": "pass"
        }))
        .expect_err("非法 kind 必须报错");
        assert!(matches!(err, ValidationError::InvalidRecord { .. }));

        let err = parse_validation_record_arguments(&serde_json::json!({
            "plan_step_id": "s1",
            "kind": "test_suite",
            "outcome": "maybe"
        }))
        .expect_err("非法 outcome 必须报错");
        assert!(matches!(err, ValidationError::InvalidRecord { .. }));

        let ok = parse_validation_record_arguments(&serde_json::json!({
            "plan_step_id": "  s1  ",
            "kind": "integration",
            "outcome": "passed",
            "command": "npm run e2e",
            "evidence": "all green"
        }))
        .expect("合法入参必须解析");
        assert_eq!(ok.plan_step_id, "s1");
        assert_eq!(ok.kind, ValidationKind::IntegrationSmoke);
        assert_eq!(ok.outcome, ValidationOutcome::Pass);
        assert_eq!(ok.command.as_deref(), Some("npm run e2e"));
        assert_eq!(ok.evidence.as_deref(), Some("all green"));
    }

    #[test]
    fn render_and_parse_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root =
            WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = ValidationStore::open_with_home(tmp.path(), &ws_root)
            .expect("open store");
        let mut report = ValidationReport::new(mission(), UtcMillis(1));
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Pass,
                command: Some("cargo test".to_string()),
                evidence: Some("16 passed".to_string()),
            },
            UtcMillis(10),
        );
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s2".to_string(),
                kind: ValidationKind::TypeCheck,
                outcome: ValidationOutcome::Fail,
                command: Some("tsc --noEmit".to_string()),
                evidence: Some("3 errors".to_string()),
            },
            UtcMillis(20),
        );
        store.save(&report).expect("save");
        let loaded = store
            .load(&report.mission_id)
            .expect("load")
            .expect("report saved");
        assert_eq!(loaded, report);
    }

    #[test]
    fn render_for_prompt_groups_by_step_and_returns_none_when_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root =
            WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = ValidationStore::open_with_home(tmp.path(), &ws_root)
            .expect("open store");

        assert!(
            store.render_for_prompt(&mission()).expect("render").is_none(),
            "空 mission 必须返回 None"
        );

        let mut report = ValidationReport::new(mission(), UtcMillis(0));
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s1".to_string(),
                kind: ValidationKind::TestSuite,
                outcome: ValidationOutcome::Pass,
                command: None,
                evidence: None,
            },
            UtcMillis(1),
        );
        apply_validation_record(
            &mut report,
            ValidationRecordArgs {
                plan_step_id: "s2".to_string(),
                kind: ValidationKind::TypeCheck,
                outcome: ValidationOutcome::Fail,
                command: None,
                evidence: Some("3 errors".to_string()),
            },
            UtcMillis(2),
        );
        store.save(&report).expect("save");

        let rendered = store
            .render_for_prompt(&report.mission_id)
            .expect("render")
            .expect("non-empty");
        assert!(rendered.contains("## Step `s1`"));
        assert!(rendered.contains("## Step `s2`"));
        assert!(rendered.contains("test_suite → **pass**"));
        assert!(rendered.contains("type_check → **fail**"));
    }

    #[test]
    fn registry_caches_store_by_workspace_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws_root = WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let registry = ValidationRunnerRegistry::new();
        let orig_home = std::env::var_os("HOME");
        // SAFETY: 测试单线程串行执行；末尾恢复 HOME，确保 sandbox 下走 fallback。
        unsafe { std::env::remove_var("HOME") };

        let s1 = registry.get_or_open(&ws_root).expect("first open");
        let s2 = registry.get_or_open(&ws_root).expect("second open");
        assert!(Arc::ptr_eq(&s1, &s2), "同 root 必须命中缓存返回同一 Arc");

        if let Some(orig) = orig_home {
            unsafe { std::env::set_var("HOME", orig) };
        }
    }
}
