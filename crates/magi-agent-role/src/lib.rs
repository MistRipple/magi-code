//! 任务系统 — AgentRole（角色定义 + 注册表 + 文件加载）。
//!
//! role 定义以 markdown 文件承载，每个文件等价于一个 role：
//! - 头部用 `---` 包围的 YAML 风格 front-matter，承载角色标识、展示信息、能力和
//!   Worker 约束；正文是角色的 system prompt。
//! - body 即 system_prompt 正文，可以是多行中文/英文/Markdown，原样塞入 LLM。
//!
//! `version` 是 schema 演进锚点：当 markdown 格式发生破坏性变化（如新增必填
//! key 或重命名既有 key），loader 可凭此字段在不解析 body 的前提下识别旧
//! 文件并触发迁移。当前内置集与默认值均为 `1`。
//!
//! 加载顺序：
//! 1. crate 内置 builtin 集（编译期 `include_str!` 嵌入，5 个代理角色 + 1 个内部主线协调器）；
//! 2. 用户角色（`~/.magi/roles/*.md`），与 builtin 共用 ID 命名空间，但不能覆盖 builtin。
//!
//! 解析失败（front-matter 缺失、字段无法识别）走 tracing warn，跳过该文件而不
//! 阻塞 daemon 启动——builtin 集解析失败因为是编译期常量，会在测试阶段就发现。
//!
//! 格式与 `~/.claude/agents/*.md` 同源（YAML front-matter + markdown body），方便
//! 手写、git diff 与人眼 review；不再使用 JSON，让所有角色文件形态保持唯一。

use magi_core::TaskKind;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

mod capability;

pub use capability::{
    ProfessionalCapability, ProfessionalCapabilityRegistry, ProfessionalCapabilitySummary,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentRoleError {
    #[error("read role file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse role file {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("role file {path} 缺少 id（文件名 {file_stem} 无效）")]
    InvalidId { path: PathBuf, file_stem: String },
    #[error("角色定义无效: {0}")]
    InvalidDefinition(String),
    #[error("角色版本冲突: {0}")]
    Conflict(String),
}

/// 单个 role 的定义。
///
/// `coordinator_mode = true` 表示该角色采用 Prompt-as-Code 主线编排模式：
/// LLM 通过 `agent_spawn` 创建代理并投递任务消息，随后通过 `agent_wait`
/// 收集代理终态结果；整个 orchestration 由 prompt 驱动，而不是
/// Code-as-Coordinator 在外层硬编码状态机。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRole {
    #[serde(default)]
    pub id: String,
    pub system_prompt: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supported_kinds: Vec<TaskKindLabel>,
    #[serde(default)]
    pub parallelism_limit: Option<u32>,
    #[serde(default)]
    pub coordinator_mode: bool,
    /// Schema 演进锚点。当 markdown front-matter 出现破坏性变化时，loader 可凭
    /// 此字段识别版本并触发迁移。默认值 = 1，对应当前 schema。
    #[serde(default = "default_role_version")]
    pub version: u32,
    /// 单个角色定义的内容版本。导入时不会携带本机 registry 版本。
    #[serde(default = "default_role_revision")]
    pub role_revision: u64,
    /// 角色定位与 UI 展示字段，和运行时 prompt 属于同一个规范定义。
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub focus: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub output_preferences: Vec<String>,
    #[serde(default)]
    pub ownerships: Vec<String>,
    #[serde(default)]
    pub insight_preferences: Vec<String>,
    /// 角色允许激活的能力。为空时仅用于旧版内置角色，按 capability registry 的角色范围推导。
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub color_token: String,
    #[serde(default)]
    pub icon: String,
}

fn default_role_version() -> u32 {
    1
}

fn default_role_revision() -> u64 {
    1
}

/// 当前唯一支持的角色 Markdown schema 版本。
pub const CURRENT_ROLE_SCHEMA_VERSION: u32 = 1;

/// 校验用户角色 ID。角色 ID 同时作为角色文件名和任务绑定键，因此必须在
/// 进入 registry 之前收敛为安全、稳定且可移植的命名。
pub fn validate_role_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err("角色 id 必须是 1-64 个字符".to_string());
    }
    if !id
        .chars()
        .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '-')
        || id.starts_with('-')
        || id.ends_with('-')
        || id.contains("--")
    {
        return Err(
            "角色 id 只能使用小写字母、数字和单个连字符，且不能以连字符开头或结尾".to_string(),
        );
    }
    Ok(())
}

/// 用字符串标签序列化 TaskKind，便于人手写 markdown front-matter。
/// 标签语义与 magi-core 的 TaskKind 保持一一对应。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKindLabel {
    LocalAgent,
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
            Self::LocalWorkflow => TaskKind::LocalWorkflow,
            Self::RemoteAgent => TaskKind::RemoteAgent,
            Self::MonitorMcp => TaskKind::MonitorMcp,
            Self::InProcessTeammate => TaskKind::InProcessTeammate,
            Self::Dream => TaskKind::Dream,
        }
    }

    fn parse(label: &str) -> Option<Self> {
        match label {
            "local_agent" => Some(Self::LocalAgent),
            "local_workflow" => Some(Self::LocalWorkflow),
            "remote_agent" => Some(Self::RemoteAgent),
            "monitor_mcp" => Some(Self::MonitorMcp),
            "in_process_teammate" => Some(Self::InProcessTeammate),
            "dream" => Some(Self::Dream),
            _ => None,
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

#[derive(Clone, Debug)]
struct RegistrySnapshot {
    roles: HashMap<String, AgentRole>,
    builtin_ids: HashSet<String>,
    capabilities: ProfessionalCapabilityRegistry,
    registry_revision: u64,
}

/// 进程内角色注册表。
///
/// 注册表对象本身在 daemon 生命周期内共享，内部只替换不可变快照。这样 API、
/// Dispatcher、Runner 和工具目录持有的同一个句柄都能看到 reload 后的角色，运行
/// 中任务仍可通过已经复制到 WorkerInfo 的 prompt 使用旧快照。
#[derive(Clone, Debug)]
pub struct AgentRoleRegistry {
    snapshot: Arc<RwLock<Arc<RegistrySnapshot>>>,
    user_role_dir: Arc<PathBuf>,
    mutation_lock: Arc<Mutex<()>>,
}

impl Default for AgentRoleRegistry {
    fn default() -> Self {
        Self::load_default()
    }
}

impl AgentRoleRegistry {
    pub fn empty() -> Self {
        let snapshot = RegistrySnapshot {
            roles: HashMap::new(),
            builtin_ids: HashSet::new(),
            capabilities: ProfessionalCapabilityRegistry::empty(),
            registry_revision: 0,
        };
        Self {
            snapshot: Arc::new(RwLock::new(Arc::new(snapshot))),
            user_role_dir: Arc::new(default_user_role_dir()),
            mutation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn from_map(map: HashMap<String, AgentRole>) -> Self {
        let builtin_ids = map.keys().cloned().collect();
        Self {
            snapshot: Arc::new(RwLock::new(Arc::new(RegistrySnapshot {
                roles: map,
                builtin_ids,
                capabilities: ProfessionalCapabilityRegistry::builtin(),
                registry_revision: 0,
            }))),
            user_role_dir: Arc::new(default_user_role_dir()),
            mutation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn from_maps(
        roles: HashMap<String, AgentRole>,
        capabilities: ProfessionalCapabilityRegistry,
    ) -> Self {
        let builtin_ids = roles.keys().cloned().collect();
        Self {
            snapshot: Arc::new(RwLock::new(Arc::new(RegistrySnapshot {
                roles,
                builtin_ids,
                capabilities,
                registry_revision: 0,
            }))),
            user_role_dir: Arc::new(default_user_role_dir()),
            mutation_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 仅构造编译期内置角色，用于生成产品模板和测试，不读取用户目录。
    pub fn builtin() -> Self {
        Self::from_maps(
            builtin_roles_map(),
            ProfessionalCapabilityRegistry::builtin(),
        )
    }

    /// 为测试或隔离运行目录指定用户角色路径；生产环境使用 `~/.magi/roles`。
    pub fn with_user_role_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.user_role_dir = Arc::new(path.into());
        self
    }

    /// 在指定状态根下保存用户角色，避免角色数据依赖进程启动用户目录；daemon
    /// 使用 `state_root/roles`，测试和隔离运行也可以复用同一规则。
    pub fn with_state_root(self, state_root: impl AsRef<Path>) -> Self {
        self.with_user_role_dir(state_root.as_ref().join("roles"))
    }

    /// 从 daemon 状态根加载内置角色和该实例的用户角色。
    pub fn load_from_state_root(state_root: impl AsRef<Path>) -> Result<Self, AgentRoleError> {
        let registry = Self::builtin().with_state_root(state_root);
        registry.reload_from_disk()?;
        Ok(registry)
    }

    /// 默认加载入口：先加载编译期 builtin，再加载 `~/.magi/roles/*.md`。
    /// 用户目录中的单个坏文件会被记录并跳过，不能阻塞 daemon 启动；同 ID 文件
    /// 永远不能覆盖 builtin。
    pub fn load_default() -> Self {
        let registry = Self::from_maps(
            builtin_roles_map(),
            ProfessionalCapabilityRegistry::load_default(),
        );
        let _ = registry.reload_from_disk();
        registry
    }

    fn current_snapshot(&self) -> Arc<RegistrySnapshot> {
        self.snapshot
            .read()
            .expect("角色 registry 读锁未被破坏")
            .clone()
    }

    fn replace_snapshot(&self, mut next: RegistrySnapshot) -> u64 {
        let mut guard = self.snapshot.write().expect("角色 registry 写锁未被破坏");
        next.registry_revision = guard.registry_revision.saturating_add(1);
        let revision = next.registry_revision;
        *guard = Arc::new(next);
        revision
    }

    fn replace_snapshot_if_current(
        &self,
        mut next: RegistrySnapshot,
        expected_revision: u64,
    ) -> Result<u64, AgentRoleError> {
        let mut guard = self.snapshot.write().expect("角色 registry 写锁未被破坏");
        if guard.registry_revision != expected_revision {
            return Err(AgentRoleError::Conflict(
                "角色注册表在保存期间发生变化，请刷新后重试".to_string(),
            ));
        }
        next.registry_revision = expected_revision.saturating_add(1);
        let revision = next.registry_revision;
        *guard = Arc::new(next);
        Ok(revision)
    }

    pub fn get(&self, role_id: &str) -> Option<AgentRole> {
        self.current_snapshot().roles.get(role_id).cloned()
    }

    pub fn contains(&self, role_id: &str) -> bool {
        self.current_snapshot().roles.contains_key(role_id)
    }

    pub fn is_builtin(&self, role_id: &str) -> bool {
        self.current_snapshot().builtin_ids.contains(role_id)
    }

    pub fn is_user_defined(&self, role_id: &str) -> bool {
        self.contains(role_id) && !self.is_builtin(role_id)
    }

    pub fn registry_revision(&self) -> u64 {
        self.current_snapshot().registry_revision
    }

    pub fn supported_task_kinds(&self, role_id: &str) -> Vec<TaskKind> {
        self.current_snapshot()
            .roles
            .get(role_id)
            .map(|r| r.supported_task_kinds())
            .unwrap_or_default()
    }

    pub fn role_supports_task_kind(&self, role_id: &str, kind: TaskKind) -> bool {
        self.supported_task_kinds(role_id).contains(&kind)
    }

    /// 判断 role 是否允许作为 agent_spawn 的目标。
    ///
    /// coordinator_mode 角色是主线编排身份，只能由 root task 使用；agent_spawn 只能派发
    /// 非 coordinator 的专业代理，避免出现“协调器派生协调器”的递归编排语义。
    pub fn is_spawnable_agent_role(&self, role_id: &str) -> bool {
        self.current_snapshot()
            .roles
            .get(role_id)
            .is_some_and(|role| {
                !role.coordinator_mode
                    && role.supported_task_kinds().contains(&TaskKind::LocalAgent)
            })
    }

    pub fn spawnable_agent_role_ids(&self) -> Vec<String> {
        let snapshot = self.current_snapshot();
        let mut ids = snapshot
            .roles
            .values()
            .filter(|role| {
                !role.coordinator_mode
                    && role.supported_task_kinds().contains(&TaskKind::LocalAgent)
            })
            .map(|role| role.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn all(&self) -> Vec<AgentRole> {
        let mut roles = self
            .current_snapshot()
            .roles
            .values()
            .cloned()
            .collect::<Vec<_>>();
        roles.sort_by(|left, right| left.id.cmp(&right.id));
        roles
    }

    pub fn capability_summaries_for_role(
        &self,
        role_id: &str,
    ) -> Vec<ProfessionalCapabilitySummary> {
        let snapshot = self.current_snapshot();
        if let Some(role) = snapshot.roles.get(role_id)
            && !role.capabilities.is_empty()
        {
            let allowed = role.capabilities.iter().collect::<HashSet<_>>();
            return snapshot
                .capabilities
                .summaries()
                .into_iter()
                .filter(|capability| allowed.contains(&capability.id))
                .collect();
        }
        snapshot.capabilities.summaries_for_role(role_id)
    }

    pub fn capability_summaries(&self) -> Vec<ProfessionalCapabilitySummary> {
        self.current_snapshot().capabilities.summaries()
    }

    pub fn capability_ids(&self) -> Vec<String> {
        self.current_snapshot().capabilities.ids()
    }

    pub fn capability_ids_for_role(&self, role_id: &str) -> Vec<String> {
        self.capability_summaries_for_role(role_id)
            .into_iter()
            .map(|capability| capability.id)
            .collect()
    }

    pub fn validate_capability_ids_for_role(
        &self,
        role_id: &str,
        capability_ids: &[String],
    ) -> Result<Vec<String>, String> {
        let snapshot = self.current_snapshot();
        let Some(role) = snapshot.roles.get(role_id) else {
            return Err(format!("代理角色不存在: {role_id}"));
        };
        if capability_ids.is_empty() {
            return Err("代理任务必须至少激活一项专业能力".to_string());
        }
        let allowed: HashSet<String> = if role.capabilities.is_empty() {
            snapshot
                .capabilities
                .ids_for_role(role_id)
                .into_iter()
                .collect()
        } else {
            role.capabilities.iter().cloned().collect()
        };
        let mut normalized = Vec::new();
        let mut seen = HashSet::new();
        for raw_id in capability_ids {
            let capability_id = raw_id.trim();
            if capability_id.is_empty() {
                return Err("专业能力 id 不能为空".to_string());
            }
            if snapshot.capabilities.get(capability_id).is_none() {
                return Err(format!("专业能力不存在: {capability_id}"));
            }
            if !allowed.contains(capability_id) {
                return Err(format!("代理角色 {role_id} 不拥有专业能力 {capability_id}"));
            }
            if seen.insert(capability_id.to_string()) {
                normalized.push(capability_id.to_string());
            }
        }
        normalized.sort();
        Ok(normalized)
    }

    pub fn compose_system_prompt(
        &self,
        role_id: &str,
        role_prompt: &str,
        capability_ids: &[String],
    ) -> Result<String, String> {
        if capability_ids.is_empty() {
            return Ok(role_prompt.trim().to_string());
        }
        let capability_ids = self.validate_capability_ids_for_role(role_id, capability_ids)?;
        self.current_snapshot()
            .capabilities
            .compose_prompt(role_id, role_prompt, &capability_ids)
    }

    /// 使用 Worker 创建时已经固定的角色提示词组合能力。该入口不重新读取角色的
    /// capability allowlist，避免角色编辑影响已经排队或运行中的 Worker；能力本身
    /// 仍必须来自当前 registry，未知能力会明确失败。
    pub fn compose_system_prompt_for_worker_snapshot(
        &self,
        role_id: &str,
        role_prompt: &str,
        capability_ids: &[String],
    ) -> Result<String, String> {
        if capability_ids.is_empty() {
            return Ok(role_prompt.trim().to_string());
        }
        self.current_snapshot()
            .capabilities
            .compose_prompt(role_id, role_prompt, capability_ids)
    }

    pub fn role_ids(&self) -> Vec<String> {
        let mut ids = self
            .current_snapshot()
            .roles
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    /// 返回首个 `coordinator_mode = true` 的角色。多个协调器角色并存时仍然有效——
    /// 调用方按 role_id 显式选用；本方法仅用于在没有显式选择时拿到默认协调器。
    pub fn default_coordinator(&self) -> Option<AgentRole> {
        self.current_snapshot()
            .roles
            .values()
            .find(|role| role.coordinator_mode)
            .cloned()
    }

    pub fn user_role_dir(&self) -> &Path {
        self.user_role_dir.as_path()
    }

    /// 从 builtin 与用户目录构造候选快照并一次性替换。坏文件会被跳过，内置 ID
    /// 冲突会被明确记录并保留内置角色。
    pub fn reload_from_disk(&self) -> Result<u64, AgentRoleError> {
        let _guard = self
            .mutation_lock
            .lock()
            .expect("角色 registry mutation lock 未被破坏");
        let mut roles = builtin_roles_map();
        let builtin_ids = roles.keys().cloned().collect::<HashSet<_>>();
        let capabilities = ProfessionalCapabilityRegistry::load_default();
        if self.user_role_dir.exists() {
            for role in load_dir(self.user_role_dir())? {
                if builtin_ids.contains(&role.id) {
                    tracing::warn!(role_id = %role.id, "用户角色 ID 与内置角色冲突，已保留内置角色");
                    continue;
                }
                if let Err(error) = validate_role_for_persistence(&role, &capabilities) {
                    tracing::warn!(role_id = %role.id, %error, "跳过不符合规范的用户角色");
                    continue;
                }
                roles.insert(role.id.clone(), role);
            }
        }
        Ok(self.replace_snapshot(RegistrySnapshot {
            roles,
            builtin_ids,
            capabilities,
            registry_revision: 0,
        }))
    }

    pub fn save_user_role(
        &self,
        role: AgentRole,
        expected_role_revision: Option<u64>,
    ) -> Result<AgentRole, AgentRoleError> {
        let _guard = self
            .mutation_lock
            .lock()
            .expect("角色 registry mutation lock 未被破坏");
        let snapshot = self.current_snapshot();
        let normalized = normalize_role(role).map_err(AgentRoleError::InvalidDefinition)?;
        validate_role_for_persistence(&normalized, &snapshot.capabilities)
            .map_err(AgentRoleError::InvalidDefinition)?;
        if snapshot.builtin_ids.contains(&normalized.id) {
            return Err(AgentRoleError::Conflict(format!(
                "角色 {} 是系统内置角色，不能被覆盖",
                normalized.id
            )));
        }
        let existing = snapshot.roles.get(&normalized.id);
        if let Some(existing) = existing {
            if !snapshot.builtin_ids.contains(&normalized.id) {
                match expected_role_revision {
                    Some(expected) if expected != existing.role_revision => {
                        return Err(AgentRoleError::Conflict(format!(
                            "角色 {} 已被其他窗口修改，请刷新后重试",
                            normalized.id
                        )));
                    }
                    None => {
                        return Err(AgentRoleError::Conflict(format!(
                            "角色 {} 已存在，请选择编辑、覆盖或另存为",
                            normalized.id
                        )));
                    }
                    _ => {}
                }
            }
        } else if expected_role_revision.is_some() {
            return Err(AgentRoleError::Conflict(format!(
                "角色 {} 不存在，不能按更新方式保存",
                normalized.id
            )));
        }
        let mut normalized = normalized;
        normalized.role_revision = existing
            .map(|role| role.role_revision.saturating_add(1))
            .unwrap_or(1);
        let path = self.user_role_dir.join(format!("{}.md", normalized.id));
        fs::create_dir_all(self.user_role_dir()).map_err(|source| AgentRoleError::Io {
            path: self.user_role_dir.clone().as_ref().clone(),
            source,
        })?;
        magi_core::fs_atomic::write_atomic(
            &path,
            serialize_role_markdown_for_storage(&normalized).into_bytes(),
        )
        .map_err(|source| AgentRoleError::Io {
            path: path.clone(),
            source,
        })?;
        let mut roles = snapshot.roles.clone();
        roles.insert(normalized.id.clone(), normalized.clone());
        self.replace_snapshot_if_current(
            RegistrySnapshot {
                roles,
                builtin_ids: snapshot.builtin_ids.clone(),
                capabilities: snapshot.capabilities.clone(),
                registry_revision: 0,
            },
            snapshot.registry_revision,
        )?;
        Ok(normalized)
    }

    pub fn delete_user_role(
        &self,
        role_id: &str,
        expected_role_revision: Option<u64>,
    ) -> Result<(), AgentRoleError> {
        let _guard = self
            .mutation_lock
            .lock()
            .expect("角色 registry mutation lock 未被破坏");
        let snapshot = self.current_snapshot();
        if snapshot.builtin_ids.contains(role_id) {
            return Err(AgentRoleError::Conflict(format!(
                "角色 {role_id} 是系统内置角色，不能删除"
            )));
        }
        let Some(existing) = snapshot.roles.get(role_id) else {
            return Err(AgentRoleError::InvalidDefinition(format!(
                "角色不存在: {role_id}"
            )));
        };
        if expected_role_revision != Some(existing.role_revision) {
            return Err(AgentRoleError::Conflict(format!(
                "角色 {role_id} 已被其他窗口修改，请刷新后重试"
            )));
        }
        let path = self.user_role_dir.join(format!("{role_id}.md"));
        if path.exists() {
            fs::remove_file(&path).map_err(|source| AgentRoleError::Io {
                path: path.clone(),
                source,
            })?;
        }
        let mut roles = snapshot.roles.clone();
        roles.remove(role_id);
        self.replace_snapshot_if_current(
            RegistrySnapshot {
                roles,
                builtin_ids: snapshot.builtin_ids.clone(),
                capabilities: snapshot.capabilities.clone(),
                registry_revision: 0,
            },
            snapshot.registry_revision,
        )?;
        Ok(())
    }
}

fn default_user_role_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
        .join("roles")
}

fn load_dir(dir: &Path) -> Result<Vec<AgentRole>, AgentRoleError> {
    let mut paths = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| AgentRoleError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        paths.push(path);
    }
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    let mut parsed = Vec::new();
    for path in paths {
        match load_file(&path) {
            Ok(role) => parsed.push((path, role)),
            Err(err) => {
                tracing::warn!(?err, file = %path.display(), "跳过 role 文件");
            }
        }
    }
    let mut counts = HashMap::<String, usize>::new();
    for (_, role) in &parsed {
        *counts.entry(role.id.clone()).or_default() += 1;
    }
    let mut out = Vec::new();
    for (path, role) in parsed {
        if counts.get(&role.id).copied().unwrap_or_default() > 1 {
            tracing::warn!(
                role_id = %role.id,
                file = %path.display(),
                "用户角色 ID 重复，已拒绝该 ID 的全部定义"
            );
            continue;
        }
        out.push(role);
    }
    Ok(out)
}

fn load_file(path: &Path) -> Result<AgentRole, AgentRoleError> {
    let raw = fs::read_to_string(path).map_err(|source| AgentRoleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut role = parse_role_markdown(&raw).map_err(|message| AgentRoleError::Parse {
        path: path.to_path_buf(),
        message,
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

/// builtin 默认角色集。用户角色与 builtin 共用 ID 命名空间；同名用户文件会被拒绝，
/// 以保证系统内置角色的提示词、能力和调度边界不可被覆盖。
///
/// 编译期通过 `include_str!` 嵌入每一个角色 .md；新增 builtin 角色必须同时
/// 在 `BUILTIN_ROLE_SOURCES` 数组里挂上一行——这里有意写成静态数组（而不是
/// build.rs / 运行时扫盘），让"哪些角色是 builtin"在源代码里 grep 即得。
fn builtin_roles_map() -> HashMap<String, AgentRole> {
    let mut map = HashMap::new();
    for (label, raw) in BUILTIN_ROLE_SOURCES {
        let role = parse_role_markdown(raw).unwrap_or_else(|err| {
            panic!("builtin role {label} 解析失败（编译期资产，应在测试阶段拦截）: {err}");
        });
        map.insert(role.id.clone(), role);
    }
    map
}

const BUILTIN_ROLE_SOURCES: &[(&str, &str)] = &[
    (
        "architect",
        include_str!("../assets/builtin-roles/architect.md"),
    ),
    (
        "executor",
        include_str!("../assets/builtin-roles/executor.md"),
    ),
    (
        "explorer",
        include_str!("../assets/builtin-roles/explorer.md"),
    ),
    (
        "reviewer",
        include_str!("../assets/builtin-roles/reviewer.md"),
    ),
    ("tester", include_str!("../assets/builtin-roles/tester.md")),
    (
        "coordinator",
        include_str!("../assets/builtin-roles/coordinator.md"),
    ),
];

/// 把 `---` 包围的 front-matter + markdown body 解析为 `AgentRole`。
///
/// front-matter 支持的字段（flat YAML，不递归）：
/// - `id: <string>`
/// - `supported_kinds: [a, b]`（中括号 + 逗号分隔，元素是 snake_case 标签）
/// - `parallelism_limit: <u32>`
/// - `coordinator_mode: true|false`
/// - `version: <u32>`（缺省 = 1）
/// - `role_revision: <u64>`（仅本地持久化文件使用，导出时省略）
/// - `display_name` / `description` / `role` / `color_token` / `icon`
/// - `focus` / `constraints` / `output_preferences` / `ownerships` /
///   `insight_preferences` / `capabilities`（均为字符串数组）
///
/// 选择"手写 mini parser"而不是引 serde_yaml 的理由：
/// 1. front-matter 字段集合是封闭已知的 5 个 key，没有递归结构；
/// 2. 引一个完整 YAML 解析器（serde_yaml 链路依赖 unmaintained 警告）只为这 5 个
///    字段，体积/编译时间不划算；
/// 3. 解析逻辑就放在本 crate 里，错误信息能直接指出"第几行哪个 key 错了"。
pub fn parse_role_markdown(raw: &str) -> Result<AgentRole, String> {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let trimmed = trimmed.trim_start();
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or_else(|| "缺少起始 `---` 行".to_string())?;
    let close_idx =
        find_close_delimiter(after_open).ok_or_else(|| "缺少结束 `---` 行".to_string())?;
    let header = &after_open[..close_idx.start];
    let body = after_open[close_idx.end..].trim_start_matches(['\n', '\r']);

    let mut id: String = String::new();
    let mut display_name = String::new();
    let mut description = String::new();
    let mut supported_kinds: Vec<TaskKindLabel> = Vec::new();
    let mut parallelism_limit: Option<u32> = None;
    let mut coordinator_mode = false;
    let mut version: u32 = default_role_version();
    let mut role_revision = default_role_revision();
    let mut role = String::new();
    let mut focus = Vec::new();
    let mut constraints = Vec::new();
    let mut output_preferences = Vec::new();
    let mut ownerships = Vec::new();
    let mut insight_preferences = Vec::new();
    let mut capabilities = Vec::new();
    let mut color_token = String::new();
    let mut icon = String::new();

    for (lineno, line) in header.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = trimmed
            .split_once(':')
            .ok_or_else(|| format!("第 {} 行不是 key: value 形式: {trimmed}", lineno + 1))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "id" => id = strip_inline_quotes(value).to_string(),
            "display_name" => display_name = strip_inline_quotes(value).to_string(),
            "description" => description = strip_inline_quotes(value).to_string(),
            "supported_kinds" => {
                supported_kinds = parse_kind_list(value, lineno + 1)?;
            }
            "parallelism_limit" => {
                if value != "null" {
                    let n: u32 = value.parse().map_err(|err| {
                        format!("第 {} 行 parallelism_limit 不是整数: {err}", lineno + 1)
                    })?;
                    parallelism_limit = Some(n);
                }
            }
            "coordinator_mode" => match value {
                "true" => coordinator_mode = true,
                "false" => coordinator_mode = false,
                other => {
                    return Err(format!(
                        "第 {} 行 coordinator_mode 仅支持 true/false，收到 {other}",
                        lineno + 1
                    ));
                }
            },
            "version" => {
                version = value
                    .parse()
                    .map_err(|err| format!("第 {} 行 version 不是整数: {err}", lineno + 1))?;
            }
            "role_revision" => {
                role_revision = value
                    .parse()
                    .map_err(|err| format!("第 {} 行 role_revision 不是整数: {err}", lineno + 1))?;
            }
            "role" => role = strip_inline_quotes(value).to_string(),
            "focus" => focus = parse_string_list(value, lineno + 1, "focus")?,
            "constraints" => constraints = parse_string_list(value, lineno + 1, "constraints")?,
            "output_preferences" => {
                output_preferences = parse_string_list(value, lineno + 1, "output_preferences")?
            }
            "ownerships" => ownerships = parse_string_list(value, lineno + 1, "ownerships")?,
            "insight_preferences" => {
                insight_preferences = parse_string_list(value, lineno + 1, "insight_preferences")?
            }
            "capabilities" => capabilities = parse_string_list(value, lineno + 1, "capabilities")?,
            "color_token" => color_token = strip_inline_quotes(value).to_string(),
            "icon" => icon = strip_inline_quotes(value).to_string(),
            other => {
                return Err(format!("第 {} 行未识别字段 `{other}`", lineno + 1));
            }
        }
    }

    let system_prompt = body.trim_end().to_string();
    if system_prompt.is_empty() {
        return Err("body 为空：缺少 system_prompt 正文".to_string());
    }

    Ok(AgentRole {
        id,
        system_prompt,
        display_name,
        description,
        supported_kinds,
        parallelism_limit,
        coordinator_mode,
        version,
        role_revision,
        role,
        focus,
        constraints,
        output_preferences,
        ownerships,
        insight_preferences,
        capabilities,
        color_token,
        icon,
    })
}

fn parse_string_list(value: &str, lineno: usize, field: &str) -> Result<Vec<String>, String> {
    parse_bracket_string_list(value, lineno, field)
}

/// 解析 front-matter 的字符串数组。
///
/// 导出格式使用 JSON 字符串数组，因此优先交给 serde_json 解析；为保留手写
/// Markdown 的便利性，再支持 `[a, b]` 简写。简写解析会在引号内忽略逗号，避免
/// `description` 或标签包含逗号时发生导入导出不等价。
fn parse_bracket_string_list(
    value: &str,
    lineno: usize,
    field: &str,
) -> Result<Vec<String>, String> {
    let value = value.trim();
    if let Ok(values) = serde_json::from_str::<Vec<String>>(value) {
        return Ok(deduplicate_strings(values));
    }
    let inner = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| format!("第 {lineno} 行 {field} 期望 [a, b] 格式，收到 {value}"))?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    let mut item_start = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, character) in inner.char_indices() {
        if let Some(current_quote) = quote {
            if current_quote == '"' && escaped {
                escaped = false;
                continue;
            }
            if current_quote == '"' && character == '\\' {
                escaped = true;
                continue;
            }
            if character == current_quote {
                quote = None;
            }
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == ',' {
            items.push(&inner[item_start..index]);
            item_start = index + character.len_utf8();
        }
    }
    if quote.is_some() {
        return Err(format!("第 {lineno} 行 {field} 含未闭合引号"));
    }
    items.push(&inner[item_start..]);

    let mut values = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            return Err(format!("第 {lineno} 行 {field} 含空数组元素"));
        }
        values.push(strip_inline_quotes(item).to_string());
    }
    Ok(deduplicate_strings(values))
}

fn deduplicate_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

/// 统一规范化角色字段，创建、编辑和导入都经过这一入口。
pub fn normalize_role(mut role: AgentRole) -> Result<AgentRole, String> {
    role.id = role.id.trim().to_string();
    role.display_name = role.display_name.trim().to_string();
    role.description = role.description.trim().to_string();
    role.role = role.role.trim().to_string();
    role.system_prompt = role.system_prompt.trim().to_string();
    role.color_token = role.color_token.trim().to_string();
    role.icon = role.icon.trim().to_string();
    normalize_string_list(&mut role.focus);
    normalize_string_list(&mut role.constraints);
    normalize_string_list(&mut role.output_preferences);
    normalize_string_list(&mut role.ownerships);
    normalize_string_list(&mut role.insight_preferences);
    normalize_string_list(&mut role.capabilities);
    if role.display_name.is_empty() {
        role.display_name = role.id.clone();
    }
    if role.role.is_empty() {
        role.role = role.display_name.clone();
    }
    if role.color_token.is_empty() {
        role.color_token = format!("agent-{}", role.id);
    }
    if role.icon.is_empty() {
        role.icon = "bot".to_string();
    }
    if role.role_revision == 0 {
        role.role_revision = 1;
    }
    Ok(role)
}

fn normalize_string_list(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain_mut(|value| {
        *value = value.trim().to_string();
        !value.is_empty() && seen.insert(value.clone())
    });
}

fn validate_role_for_persistence(
    role: &AgentRole,
    capabilities: &ProfessionalCapabilityRegistry,
) -> Result<(), String> {
    validate_role_id(&role.id)?;
    if role.version != CURRENT_ROLE_SCHEMA_VERSION {
        return Err(format!(
            "角色 schema version 仅支持 {}，收到 {}",
            CURRENT_ROLE_SCHEMA_VERSION, role.version
        ));
    }
    if role.display_name.is_empty() || role.display_name.chars().count() > 80 {
        return Err("角色显示名称必须是 1-80 个字符".to_string());
    }
    if role.description.chars().count() > 300 {
        return Err("角色描述不能超过 300 个字符".to_string());
    }
    if role.system_prompt.is_empty() || role.system_prompt.chars().count() > 50_000 {
        return Err("角色提示词必须非空且不超过 50000 个字符".to_string());
    }
    if role.supported_kinds.is_empty() || !role.supported_kinds.contains(&TaskKindLabel::LocalAgent)
    {
        return Err("用户角色必须支持 local_agent 任务类型".to_string());
    }
    if role.coordinator_mode {
        return Err("用户角色不能启用 coordinator_mode".to_string());
    }
    if role.parallelism_limit == Some(0) {
        return Err("parallelism_limit 必须大于 0".to_string());
    }
    if role.capabilities.is_empty() {
        return Err("用户角色至少需要一项专业能力".to_string());
    }
    for capability_id in &role.capabilities {
        if capabilities.get(capability_id).is_none() {
            return Err(format!("专业能力不存在: {capability_id}"));
        }
    }
    if role.insight_preferences.iter().any(|value| {
        !matches!(
            value.as_str(),
            "decision" | "contract" | "risk" | "constraint"
        )
    }) {
        return Err("insight_preferences 只支持 decision、contract、risk、constraint".to_string());
    }
    Ok(())
}

/// 将规范角色序列化为导出 Markdown。导出只写角色定义，不写本机 revision、引擎、
/// 凭据或运行时状态。
pub fn serialize_role_markdown(role: &AgentRole) -> String {
    serialize_role_markdown_inner(role, false)
}

/// 将规范角色序列化为本地持久化 Markdown。`role_revision` 仅用于本机并发编辑检测，
/// 不属于跨实例导出的角色内容。
fn serialize_role_markdown_for_storage(role: &AgentRole) -> String {
    serialize_role_markdown_inner(role, true)
}

fn serialize_role_markdown_inner(role: &AgentRole, include_role_revision: bool) -> String {
    let quote = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    let list = |values: &[String]| {
        values
            .iter()
            .map(|value| quote(value))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let kinds = role
        .supported_kinds
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let role_revision = include_role_revision
        .then(|| format!("role_revision: {}\n", role.role_revision))
        .unwrap_or_default();
    format!(
        "---\nid: {}\ndisplay_name: {}\ndescription: {}\nsupported_kinds: [{}]\nparallelism_limit: {}\ncoordinator_mode: {}\nversion: {}\n{}role: {}\nfocus: [{}]\nconstraints: [{}]\noutput_preferences: [{}]\nownerships: [{}]\ninsight_preferences: [{}]\ncapabilities: [{}]\ncolor_token: {}\nicon: {}\n---\n{}\n",
        role.id,
        quote(&role.display_name),
        quote(&role.description),
        kinds,
        role.parallelism_limit
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        role.coordinator_mode,
        role.version,
        role_revision,
        quote(&role.role),
        list(&role.focus),
        list(&role.constraints),
        list(&role.output_preferences),
        list(&role.ownerships),
        list(&role.insight_preferences),
        list(&role.capabilities),
        quote(&role.color_token),
        quote(&role.icon),
        role.system_prompt.trim(),
    )
}

impl TaskKindLabel {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalAgent => "local_agent",
            Self::LocalWorkflow => "local_workflow",
            Self::RemoteAgent => "remote_agent",
            Self::MonitorMcp => "monitor_mcp",
            Self::InProcessTeammate => "in_process_teammate",
            Self::Dream => "dream",
        }
    }
}

struct DelimRange {
    start: usize,
    end: usize,
}

fn find_close_delimiter(after_open: &str) -> Option<DelimRange> {
    // 在 header 区段里寻找单独成行的 `---`，兼容 LF / CRLF。
    let mut search_from = 0usize;
    while let Some(rel) = after_open[search_from..].find("---") {
        let abs = search_from + rel;
        let starts_at_line = abs == 0 || matches!(after_open.as_bytes()[abs - 1], b'\n');
        if starts_at_line {
            let after = abs + 3;
            let bytes = after_open.as_bytes();
            let line_end_ok = match bytes.get(after) {
                None => true,
                Some(b'\n') => true,
                Some(b'\r') if matches!(bytes.get(after + 1), Some(b'\n')) => true,
                _ => false,
            };
            if line_end_ok {
                let end_consumed = match bytes.get(after) {
                    Some(b'\n') => after + 1,
                    Some(b'\r') if bytes.get(after + 1) == Some(&b'\n') => after + 2,
                    _ => after,
                };
                return Some(DelimRange {
                    start: abs,
                    end: end_consumed,
                });
            }
        }
        search_from = abs + 3;
    }
    None
}

fn strip_inline_quotes(value: &str) -> Cow<'_, str> {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        serde_json::from_str(value)
            .unwrap_or_else(|_| value[1..value.len() - 1].to_string())
            .into()
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].to_string().into()
    } else {
        Cow::Borrowed(value)
    }
}

fn parse_kind_list(value: &str, lineno: usize) -> Result<Vec<TaskKindLabel>, String> {
    let labels = parse_bracket_string_list(value, lineno, "supported_kinds")?;
    let mut out = Vec::new();
    for p in labels {
        let label = TaskKindLabel::parse(&p)
            .ok_or_else(|| format!("第 {lineno} 行 supported_kinds 含未识别标签 `{p}`"))?;
        out.push(label);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_set_covers_core_roles() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for role in [
            "architect",
            "executor",
            "reviewer",
            "explorer",
            "tester",
            "coordinator",
        ] {
            assert!(reg.get(role).is_some(), "missing builtin role: {role}");
        }
    }

    #[test]
    fn builtin_roles_expose_professional_capabilities() {
        let registry = AgentRoleRegistry::from_map(builtin_roles_map());
        assert!(registry.capability_ids().contains(&"frontend".to_string()));
        for role_id in ["architect", "executor", "reviewer", "explorer", "tester"] {
            let ids = registry.capability_ids_for_role(role_id);
            assert!(
                ids.contains(&"general_engineering".to_string()),
                "{role_id} 缺少通用工程能力"
            );
            assert!(
                ids.contains(&"frontend".to_string()),
                "{role_id} 缺少前端能力"
            );
        }
    }

    #[test]
    fn coordinator_routes_web_tasks_to_browser_capable_workers() {
        let registry = AgentRoleRegistry::from_map(builtin_roles_map());
        let coordinator = registry
            .get("coordinator")
            .expect("coordinator role exists");

        for expected in [
            "Web 网站",
            "`frontend`",
            "`quality_engineering`",
            "`browser_*`",
            "真实页面操作",
        ] {
            assert!(
                coordinator.system_prompt.contains(expected),
                "coordinator 缺少 Web Worker 路由契约 {expected}: {}",
                coordinator.system_prompt
            );
        }
    }

    #[test]
    fn architect_supports_local_agent() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("architect").expect("architect role exists");
        let kinds = role.supported_task_kinds();
        assert!(kinds.contains(&TaskKind::LocalAgent));
    }

    #[test]
    fn architect_returns_decision_requests_to_mainline() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("architect").expect("architect role exists");
        assert!(
            role.system_prompt.contains("返回主线"),
            "architect 不能绕过主线直接面向用户，实际: {}",
            role.system_prompt
        );
        assert!(!role.system_prompt.contains("唯一代言人"));
    }

    #[test]
    fn builtin_agent_prompts_define_non_recursive_worker_boundary() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for id in ["architect", "executor", "reviewer", "explorer", "tester"] {
            let role = reg.get(id).expect("builtin role exists");
            assert!(
                role.system_prompt.contains("不能创建更多子代理"),
                "{id} 必须明确禁止递归派发代理"
            );
            assert!(
                role.system_prompt.contains("返回主线"),
                "{id} 必须明确结果回传给主线"
            );
        }
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
    fn coordinator_prompt_keeps_mainline_execution_responsibility() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        let role = reg.get("coordinator").expect("coordinator role exists");
        assert!(
            role.system_prompt
                .contains("可以直接分析、读取、编辑、运行命令、验证并总结"),
            "coordinator 必须保留主线亲自推进能力，实际: {}",
            role.system_prompt
        );
        assert!(
            !role
                .system_prompt
                .contains("不要直接编辑代码、不要直接跑测试"),
            "coordinator 不能被弱化成只派活不执行的空壳角色"
        );
    }

    #[test]
    fn non_coordinator_roles_have_coordinator_mode_false_by_default() {
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for id in ["architect", "executor", "reviewer", "explorer"] {
            let role = reg.get(id).expect("builtin role exists");
            assert!(
                !role.coordinator_mode,
                "{id} should NOT default to coordinator_mode",
            );
        }
    }

    #[test]
    fn parse_role_markdown_handles_all_fields() {
        let raw = "---\nid: ml-engineer\nsupported_kinds: [local_agent, local_workflow]\nparallelism_limit: 2\ncoordinator_mode: false\nversion: 1\n---\n你是机器学习工程师\n";
        let role = parse_role_markdown(raw).expect("解析成功");
        assert_eq!(role.id, "ml-engineer");
        assert_eq!(role.system_prompt, "你是机器学习工程师");
        assert_eq!(role.parallelism_limit, Some(2));
        assert_eq!(role.supported_task_kinds().len(), 2);
        assert!(!role.coordinator_mode);
        assert_eq!(role.version, 1);
    }

    #[test]
    fn parse_role_markdown_defaults_version_to_one() {
        // 缺省 version 字段时回落到 1，保证既有 builtin 文件不需要全量加 version 也可解析。
        let raw = "---\nid: legacy\nsupported_kinds: [local_agent]\n---\n你是 legacy\n";
        let role = parse_role_markdown(raw).expect("解析成功");
        assert_eq!(role.version, 1);
    }

    #[test]
    fn parse_role_markdown_rejects_non_integer_version() {
        let err = parse_role_markdown(
            "---\nid: foo\nsupported_kinds: [local_agent]\nversion: v1\n---\n你是 foo\n",
        )
        .expect_err("version 非整数应失败");
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_unsupported_schema_version() {
        let registry = AgentRoleRegistry::builtin();
        let role = parse_role_markdown(
            "---\nid: foo\nsupported_kinds: [local_agent]\nversion: 2\n---\n你是 foo\n",
        )
        .expect("解析阶段保留版本信息");
        let error = validate_role_for_persistence(&role, &registry.current_snapshot().capabilities)
            .expect_err("未知 schema version 应拒绝持久化");
        assert!(error.contains("schema version"), "{error}");
    }

    #[test]
    fn parse_string_list_preserves_commas_and_json_escapes() {
        let raw = "---\nid: analyst\ndisplay_name: \"Data, Analyst\"\ndescription: \"描述\"\nsupported_kinds: [local_agent]\nfocus: [\"读取,清洗\", \"结果\\\"核对\"]\n---\n分析\n";
        let role = parse_role_markdown(raw).expect("带逗号的列表应可解析");
        assert_eq!(role.display_name, "Data, Analyst");
        assert_eq!(role.focus, vec!["读取,清洗", "结果\"核对"]);
    }

    #[test]
    fn builtin_roles_all_at_version_one() {
        // 守护内置集与代码默认版本同步——schema 升版时必须同步改这里。
        let reg = AgentRoleRegistry::from_map(builtin_roles_map());
        for role in reg.all() {
            assert_eq!(
                role.version, 1,
                "builtin role {} version 应为 1，实际 {}",
                role.id, role.version
            );
        }
    }

    #[test]
    fn parse_role_markdown_rejects_missing_front_matter() {
        let err = parse_role_markdown("你是 X").expect_err("缺 front-matter 应失败");
        assert!(err.contains("起始"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_unclosed_front_matter() {
        let err = parse_role_markdown("---\nid: foo\n你是 X\n").expect_err("缺尾部 --- 应失败");
        assert!(err.contains("结束"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_empty_body() {
        let err = parse_role_markdown("---\nid: foo\n---\n").expect_err("空 body 应失败");
        assert!(err.contains("body"), "{err}");
    }

    #[test]
    fn parse_role_markdown_rejects_unknown_kind() {
        let err =
            parse_role_markdown("---\nid: foo\nsupported_kinds: [magic_thing]\n---\n你是 foo\n")
                .expect_err("未识别 kind 应失败");
        assert!(err.contains("magic_thing"), "{err}");
    }

    #[test]
    fn load_file_parses_markdown() {
        let dir = tempdir();
        let path = dir.join("ml-engineer.md");
        fs::write(
            &path,
            "---\nid: ml-engineer\nsupported_kinds: [local_agent, local_workflow]\nparallelism_limit: 2\n---\n你是机器学习工程师\n",
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
        let path = dir.join("custom.md");
        fs::write(
            &path,
            "---\nid: \"\"\nsupported_kinds: [local_agent]\n---\n你是 custom\n",
        )
        .unwrap();
        let role = load_file(&path).unwrap();
        assert_eq!(role.id, "custom");
    }

    #[test]
    fn user_role_round_trip_and_reload_are_atomic() {
        let dir = tempdir();
        let registry = AgentRoleRegistry::builtin().with_user_role_dir(&dir);
        let mut role = registry.get("executor").expect("builtin role exists");
        role.id = "data-analyst".to_string();
        role.display_name = "数据分析师".to_string();
        role.description = "负责数据分析".to_string();
        role.system_prompt = "你是数据分析师。".to_string();
        role.role = "数据分析与验证".to_string();
        role.capabilities = vec![
            "general_engineering".to_string(),
            "data_engineering".to_string(),
        ];
        role.role_revision = 1;

        let saved = registry
            .save_user_role(role.clone(), None)
            .expect("保存用户角色");
        assert_eq!(saved.role_revision, 1);
        assert!(registry.is_user_defined("data-analyst"));
        let serialized = serialize_role_markdown(&saved);
        assert!(
            !serialized.contains("role_revision:"),
            "导出不能携带本机 revision"
        );
        let parsed = normalize_role(parse_role_markdown(&serialized).expect("导出内容可解析"))
            .expect("导出内容可规范化");
        assert_eq!(parsed.id, "data-analyst");
        assert_eq!(parsed.system_prompt, "你是数据分析师。");
        assert_eq!(parsed.capabilities, role.capabilities);

        let reloaded = AgentRoleRegistry::builtin().with_user_role_dir(&dir);
        reloaded.reload_from_disk().expect("重载用户角色");
        let reloaded_role = reloaded.get("data-analyst").expect("角色应恢复");
        assert_eq!(reloaded_role.display_name, "数据分析师");
        assert_eq!(reloaded_role.role_revision, 1);
        let storage = fs::read_to_string(dir.join("data-analyst.md")).expect("读取本地角色文件");
        assert!(storage.contains("role_revision: 1"));
    }

    #[test]
    fn load_from_state_root_restores_user_roles_from_instance_directory() {
        let state_root = tempdir();
        let registry =
            AgentRoleRegistry::load_from_state_root(&state_root).expect("状态根角色注册表应加载");
        let mut role = registry.get("executor").expect("builtin role exists");
        role.id = "state-root-role".to_string();
        role.display_name = "状态根角色".to_string();
        registry.save_user_role(role, None).expect("用户角色应保存");

        let reloaded = AgentRoleRegistry::load_from_state_root(&state_root)
            .expect("重启后状态根角色注册表应加载");
        assert!(reloaded.is_user_defined("state-root-role"));
    }

    #[test]
    fn serializer_preserves_coordinator_mode() {
        let registry = AgentRoleRegistry::builtin();
        let coordinator = registry
            .get("coordinator")
            .expect("coordinator role exists");
        let exported = serialize_role_markdown(&coordinator);
        assert!(exported.contains("coordinator_mode: true"));
    }

    #[test]
    fn builtin_role_cannot_be_overwritten_or_deleted() {
        let dir = tempdir();
        let registry = AgentRoleRegistry::builtin().with_user_role_dir(&dir);
        let role = registry.get("executor").expect("builtin role exists");
        assert!(matches!(
            registry.save_user_role(role, Some(1)),
            Err(AgentRoleError::Conflict(_))
        ));
        assert!(matches!(
            registry.delete_user_role("executor", Some(1)),
            Err(AgentRoleError::Conflict(_))
        ));
    }

    #[test]
    fn concurrent_revision_conflict_keeps_latest_user_role() {
        let dir = tempdir();
        let registry = AgentRoleRegistry::builtin().with_user_role_dir(&dir);
        let mut role = registry.get("executor").expect("builtin role exists");
        role.id = "auditor".to_string();
        role.display_name = "审计员".to_string();
        role.system_prompt = "审计任务。".to_string();
        registry
            .save_user_role(role.clone(), None)
            .expect("创建角色");
        role.description = "第一次更新".to_string();
        registry
            .save_user_role(role.clone(), Some(1))
            .expect("第一次更新");
        role.description = "过期更新".to_string();
        assert!(matches!(
            registry.save_user_role(role, Some(1)),
            Err(AgentRoleError::Conflict(_))
        ));
        assert_eq!(registry.get("auditor").unwrap().description, "第一次更新");
    }

    #[test]
    fn reload_rejects_duplicate_user_role_ids_across_different_files() {
        let dir = tempdir();
        let content = |prompt: &str| {
            format!(
                "---\nid: duplicate-role\ndisplay_name: 角色\nsupported_kinds: [local_agent]\ncapabilities: [general_engineering]\n---\n{prompt}\n"
            )
        };
        fs::write(dir.join("first.md"), content("第一份定义")).expect("第一份角色应写入");
        fs::write(dir.join("second.md"), content("第二份定义")).expect("第二份角色应写入");

        let registry = AgentRoleRegistry::builtin().with_user_role_dir(&dir);
        registry
            .reload_from_disk()
            .expect("重复角色文件不应阻塞 reload");

        assert!(
            !registry.contains("duplicate-role"),
            "相同 ID 的多个用户定义必须全部拒绝，不能按文件名随机选一个"
        );
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
