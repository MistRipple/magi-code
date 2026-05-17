//! Task System v2 — AgentRole（角色定义 + 注册表 + 文件加载）。
//!
//! role 定义从 `~/.magi/roles/*.json` 加载，找不到文件时使用 crate 内置默认集。
//!
//! 设计目标：
//! - 一份 role 定义对应一个 JSON 文件，文件名（除 `.json`）即 role id。
//! - 加载顺序：先 user override（`~/.magi/roles/*.json`），再 builtin defaults
//!   （内嵌字符串），同 id 时 user override 优先。
//! - 加载失败（解析错误、IO 错误）记入 tracing 警告，跳过该文件——不阻塞启动。
//!
//! 文件格式选择：原设计 TOML，受当前 workspace 依赖封闭性限制使用 JSON（与
//! `~/.claude/agents/*.md`+YAML 不同的另一条路径，但语义等价）。后续如需切回
//! TOML，仅本 crate 调换 parser 即可，不影响调用方。

use magi_core::TaskKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentRoleError {
    #[error("read role file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse role file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("role file {path} 缺少 id（文件名 {file_stem} 无效）")]
    InvalidId { path: PathBuf, file_stem: String },
}

/// 单个 role 的定义。
///
/// `coordinator_mode = true` 表示该角色采用 Prompt-as-Code 协调器模式：
/// LLM 通过 `Agent` / `SendMessage` / `TaskStop` 三个内置工具发起子代理派发与
/// 跨任务消息传递，整个 orchestration 由 prompt 驱动，而不是 Code-as-Coordinator
/// 在外层硬编码状态机。架构详见 docs/task-system-v2/01-architecture.md L10。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRole {
    #[serde(default)]
    pub id: String,
    pub system_prompt: String,
    #[serde(default)]
    pub supported_kinds: Vec<TaskKindLabel>,
    #[serde(default)]
    pub parallelism_limit: Option<u32>,
    #[serde(default)]
    pub coordinator_mode: bool,
}

/// 用字符串标签序列化 TaskKind，便于人手写 JSON。
/// 标签语义与 magi-core 的 TaskKind 保持一一对应。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindLabel {
    LocalAgent,
    LocalBash,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}

impl TaskKindLabel {
    pub fn to_task_kind(self) -> TaskKind {
        match self {
            Self::LocalAgent => TaskKind::LocalAgent,
            Self::LocalBash => TaskKind::LocalBash,
            Self::LocalWorkflow => TaskKind::LocalWorkflow,
            Self::RemoteAgent => TaskKind::RemoteAgent,
            Self::MonitorMcp => TaskKind::MonitorMcp,
            Self::InProcessTeammate => TaskKind::InProcessTeammate,
            Self::Dream => TaskKind::Dream,
        }
    }
}

impl AgentRole {
    pub fn supported_task_kinds(&self) -> Vec<TaskKind> {
        self.supported_kinds
            .iter()
            .map(|k| k.to_task_kind())
            .collect()
    }
}

/// 进程内角色注册表。注册表本身只读不可变；运行时若要刷新（reload），重新
/// 构造一个新的 Arc 并替换即可。
#[derive(Clone, Debug, Default)]
pub struct AgentRoleRegistry {
    roles: Arc<HashMap<String, AgentRole>>,
}

impl AgentRoleRegistry {
    pub fn empty() -> Self {
        Self {
            roles: Arc::new(HashMap::new()),
        }
    }

    pub fn from_map(map: HashMap<String, AgentRole>) -> Self {
        Self {
            roles: Arc::new(map),
        }
    }

    /// 默认加载入口：先 user override (`~/.magi/roles/*.json`)，再 builtin defaults。
    /// 用户路径不存在或读取失败不报错，自动回落到 builtin 集——这是 daemon 启动
    /// 路径，绝不阻塞。
    pub fn load_default() -> Self {
        let mut map = builtin_roles_map();
        if let Some(dir) = user_role_dir() {
            if dir.exists() {
                match load_dir(&dir) {
                    Ok(user_roles) => {
                        for role in user_roles {
                            map.insert(role.id.clone(), role);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, dir = %dir.display(), "加载 ~/.magi/roles 失败，使用 builtin 默认集");
                    }
                }
            }
        }
        Self::from_map(map)
    }

    pub fn get(&self, role_id: &str) -> Option<&AgentRole> {
        self.roles.get(role_id)
    }

    pub fn contains(&self, role_id: &str) -> bool {
        self.roles.contains_key(role_id)
    }

    pub fn supported_task_kinds(&self, role_id: &str) -> Vec<TaskKind> {
        self.roles
            .get(role_id)
            .map(|r| r.supported_task_kinds())
            .unwrap_or_default()
    }

    pub fn role_supports_task_kind(&self, role_id: &str, kind: TaskKind) -> bool {
        self.supported_task_kinds(role_id).contains(&kind)
    }

    pub fn all(&self) -> impl Iterator<Item = &AgentRole> {
        self.roles.values()
    }

    pub fn role_ids(&self) -> Vec<String> {
        self.roles.keys().cloned().collect()
    }

    /// 返回首个 `coordinator_mode = true` 的角色。多个协调器角色并存时仍然有效——
    /// 调用方按 role_id 显式选用；本方法仅用于在没有显式选择时拿到默认协调器。
    pub fn default_coordinator(&self) -> Option<&AgentRole> {
        self.roles.values().find(|role| role.coordinator_mode)
    }
}

fn user_role_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".magi").join("roles"))
}

fn load_dir(dir: &Path) -> Result<Vec<AgentRole>, AgentRoleError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| AgentRoleError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        match load_file(&path) {
            Ok(role) => out.push(role),
            Err(err) => {
                tracing::warn!(?err, file = %path.display(), "跳过 role 文件");
            }
        }
    }
    Ok(out)
}

fn load_file(path: &Path) -> Result<AgentRole, AgentRoleError> {
    let raw = fs::read_to_string(path).map_err(|source| AgentRoleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut role: AgentRole =
        serde_json::from_str(&raw).map_err(|source| AgentRoleError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if role.id.trim().is_empty() {
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if file_stem.is_empty() {
            return Err(AgentRoleError::InvalidId {
                path: path.to_path_buf(),
                file_stem,
            });
        }
        role.id = file_stem;
    }
    Ok(role)
}

/// builtin 默认角色集。用户在 `~/.magi/roles/<id>.json` 提供同名文件即可覆盖。
fn builtin_roles_map() -> HashMap<String, AgentRole> {
    let raw = include_str!("../assets/builtin-roles.json");
    let parsed: BuiltinRolesFile =
        serde_json::from_str(raw).expect("builtin-roles.json 解析失败（编译期资产，不该发生）");
    let mut map = HashMap::new();
    for role in parsed.roles {
        map.insert(role.id.clone(), role);
    }
    map
}

#[derive(Deserialize)]
struct BuiltinRolesFile {
    roles: Vec<AgentRole>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_set_covers_core_roles() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for role in [
            "architect",
            "integration-dev",
            "reviewer",
            "debugger",
            "frontend-dev",
            "backend-dev",
        ] {
            assert!(reg.get(role).is_some(), "missing builtin role: {role}");
        }
    }

    #[test]
    fn architect_supports_objective_phase_workpackage() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("architect").expect("architect role exists");
        let kinds = role.supported_task_kinds();
        assert!(kinds.contains(&TaskKind::LocalAgent));
        assert!(kinds.contains(&TaskKind::LocalAgent));
        assert!(kinds.contains(&TaskKind::LocalAgent));
    }

    #[test]
    fn role_supports_task_kind_handles_missing_role() {
        let reg = AgentRoleRegistry::empty();
        assert!(!reg.role_supports_task_kind("missing", TaskKind::LocalAgent));
    }

    #[test]
    fn builtin_set_contains_coordinator_with_coordinator_mode() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("coordinator").expect("coordinator role exists");
        assert!(
            role.coordinator_mode,
            "coordinator role must enable coordinator_mode"
        );
        let default = reg
            .default_coordinator()
            .expect("default_coordinator resolves");
        assert_eq!(default.id, "coordinator");
    }

    #[test]
    fn non_coordinator_roles_have_coordinator_mode_false_by_default() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for id in ["architect", "integration-dev", "reviewer", "debugger"] {
            let role = reg.get(id).expect("builtin role exists");
            assert!(
                !role.coordinator_mode,
                "{id} should NOT default to coordinator_mode",
            );
        }
    }

    #[test]
    fn load_file_parses_minimal_json() {
        let dir = tempdir();
        let path = dir.join("ml-engineer.json");
        fs::write(
            &path,
            r#"{"id":"ml-engineer","system_prompt":"你是机器学习工程师","supported_kinds":["local_agent","local_bash"],"parallelism_limit":2}"#,
        )
        .unwrap();
        let role = load_file(&path).unwrap();
        assert_eq!(role.id, "ml-engineer");
        assert_eq!(role.parallelism_limit, Some(2));
        assert_eq!(role.supported_task_kinds().len(), 2);
    }

    #[test]
    fn load_file_infers_id_from_filename() {
        let dir = tempdir();
        let path = dir.join("custom.json");
        fs::write(&path, r#"{"id":"","system_prompt":"test"}"#).unwrap();
        let role = load_file(&path).unwrap();
        assert_eq!(role.id, "custom");
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "magi-agent-role-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }
}
